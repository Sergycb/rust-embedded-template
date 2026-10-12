//! `cargo xtask panic-info` — причина последнего падения, снятая с платы.

use anyhow::Context;
use xshell::cmd;

use crate::memory::{app_memory_regions, region};
use crate::project::CHIP;

/// Магия, которой `panic-persist` помечает сохранённый дамп (первое слово
/// региона `PANIC`, дальше длина сообщения и его байты). Значение — из самого
/// крейта: считать формат приходится здесь, потому что читает его хост, а
/// прошивке в этот момент, как правило, не до того.
const PANIC_MAGIC: u32 = 0x0FAC_ADE0;

/// Причина последнего падения, снятая с платы без перепрошивки и без RTT-сессии.
///
/// Зачем отдельная команда, если `main` и так печатает дамп при старте: чтобы
/// увидеть эту печать, нужен пробник, подключённый **в момент** старта. Под
/// отладкой же плата после падения не стартует вовсе — паникёр заканчивается
/// `udf()`, то есть остановкой на месте, — и дамп лежит в RAM, пока его никто
/// не прочитал. Этот случай команда и закрывает: пришли к зависшей плате,
/// спросили причину.
///
/// Прямой `probe-rs read` этого не делает — он отдал бы hex-дамп региона, из
/// которого строку ещё надо собрать, зная формат `panic-persist`: магия по
/// смещению 0, длина по 4, байты с 8.
///
/// Ограничение прямое следствие того же: если приложение успело стартовать
/// (release-профиль, где хендлер делает `sys_reset`), оно дамп уже вычитало и
/// магию стёрло — `panic-persist` так устроен намеренно, чтобы одно падение
/// не показывалось вечно. Тогда причина есть только в defmt-логе того старта.
/// Нужно иначе — заведите свой регион в раскладке (как `PANIC`, через
/// `chip-data-gen`/`chip-select.rhai`), копируйте причину туда при старте и
/// читайте её отсюда; в шаблоне этого нет, потому что за него платит каждый
/// проект, а нужно оно не всем.
pub fn panic_info(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let regions = app_memory_regions()?;
    let panic = region(&regions, "PANIC").context(
        "в crates-cross/app/memory.x нет региона PANIC — на этом чипе дамп паники негде хранить",
    )?;

    // Заголовок: магия и длина сообщения. Разбор строгий (см. [`parse_hex`]):
    // «плата не падала» и «я не понял вывод probe-rs» обязаны быть разными
    // ответами, а при `_ => 0` они были одним.
    let header = format!("{:#x}", panic.origin);
    let header = cmd!(sh, "probe-rs read --chip {CHIP} b32 {header} 2").read()?;
    let words = header
        .split_whitespace()
        .map(parse_hex)
        .collect::<Option<Vec<_>>>()
        .with_context(|| {
            format!(
                "не разобрал вывод `probe-rs read` ({header:?}) — ожидался голый hex-список слов"
            )
        })?;
    let [magic, length] = words[..] else {
        anyhow::bail!(
            "из `probe-rs read` вернулось {} слов, а ожидались магия и длина ({:?})",
            words.len(),
            header,
        );
    };

    if magic != PANIC_MAGIC {
        println!("дампа нет: в начале PANIC не {PANIC_MAGIC:#010x}, а {magic:#010x}.");
        // Сырой ответ рядом с разобранным: обрезанное слово (`0FACADE` вместо
        // `0FACADE0`) разбирается в другое число, и без исходной строки «плата не
        // падала» от «probe-rs напечатал не то» не отличить.
        println!("Ответ probe-rs: {}", header.trim());
        println!(
            "Либо плата не падала, либо приложение уже стартовало и вычитало дамп — тогда \
             причина ушла в defmt-лог того запуска."
        );
        return Ok(());
    }

    // Восемь байт заголовка не входят в сообщение. Длину всё равно
    // проверяем: в регионе могло оказаться что угодно, а `probe-rs read` с
    // мусорным числом слов ждал бы долго и молча.
    let available = panic.length.saturating_sub(8);
    if length == 0 || u64::from(length) > available {
        anyhow::bail!(
            "магия на месте, но длина сообщения ({length}) не помещается в PANIC ({available} \
             байт) — дамп повреждён"
        );
    }

    let start = format!("{:#x}", panic.origin + 8);
    let wanted = length;
    let words = wanted.to_string();
    let body = cmd!(sh, "probe-rs read --chip {CHIP} b8 {start} {words}").read()?;
    // Строго, как заголовок: молчаливый `0x00` вместо непрочитанного слова дал бы
    // обрезанное сообщение паники с ВЫГЛЯДЯЩИМ выводом, а это худший вид
    // ответа — читатель поверит ему и ищет не ту причину.
    let bytes = body
        .split_whitespace()
        .map(parse_hex)
        .collect::<Option<Vec<_>>>()
        .with_context(|| {
            format!("не разобрал вывод `probe-rs read` ({body:?}) — ожидался голый hex-список байт")
        })?
        .into_iter()
        .map(|byte| {
            u8::try_from(byte).with_context(|| {
                format!("`probe-rs read` вернул слово {byte:#x} — байтом оно не является")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        u32::try_from(bytes.len()).is_ok_and(|v| v == wanted),
        "из PANIC прочитано {} байт вместо {wanted} — дамп собран не полностью",
        bytes.len(),
    );

    let dump = String::from_utf8_lossy(&bytes);
    println!("причина последнего падения ({} байт):", bytes.len());
    println!("{dump}");

    Ok(())
}

/// Слово из вывода `probe-rs read`: hex без префикса.
///
/// `Option`, а не число с запасным нулём. При разборе дампа паники ноль —
/// законное значение, и подставив его вместо непрочитанного слова, мы получили
/// бы два разных ответа («плата не падала» и «я не понял вывод `probe-rs`»)
/// одним и тем же `println!` с кодом выхода 0. Второй ответ при отладке нужен
/// не меньше первого.
///
/// Формат сверен с probe-rs 0.31: `read_to_console` печатает голый
/// hex-список слов. На `master` формат по умолчанию другой — `HexTable` с
/// префиксом `{address:08x}: `, — и такой вывод разбор принимать не должен,
/// а переписывать разбор под него надо вместе с проверкой на реальной плате.
fn parse_hex(word: &str) -> Option<u32> {
    // `from_str_radix` принимает ведущий `+`, а слово из `probe-rs read` — только цифры.
    if word.is_empty() || !word.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(word, 16).ok()
}

#[cfg(test)]
mod tests {
    /// `+` перед числом — не hex-цифра.
    #[test]
    fn parse_hex_rejects_signs() {
        assert_eq!(super::parse_hex("+1f"), None);
    }

    /// Слово из `probe-rs read` обязано разбираться строго: при подстановке нуля
    /// вместо непрочитанного слова «плата не падала» и «я не понял вывод
    /// probe-rs» стали бы одним ответом с кодом выхода 0.
    #[test]
    fn parses_only_real_words_of_probe_rs_output() {
        assert_eq!(super::parse_hex("0facade0"), Some(0x0FAC_ADE0));
        assert_eq!(super::parse_hex("0"), Some(0));

        // Префикс таблицы, который печатает probe-rs на `master`, и обрезок
        // слова — обе подстановки молча дали бы «нули» вместо ошибки.
        assert_eq!(super::parse_hex("0800_0000:"), None);
        assert_eq!(super::parse_hex("0100"), Some(0x0100));
        assert_eq!(super::parse_hex(""), None);
    }
}
