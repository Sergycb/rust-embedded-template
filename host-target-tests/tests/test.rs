//! Тесты, где хост управляет уже прошитым устройством
//! (`cargo xtask test host-target`).
//!
//! Третий этап трёхэтапной модели. Отличие от двух других — в том, кто кем
//! командует: `cargo xtask test host` гоняет логику `domain` на хосте без
//! всякого железа, `cargo xtask test target` выполняет код *внутри* МК, а
//! здесь тест — обычная host-программа, которая смотрит на устройство
//! снаружи и общается с ним так же, как это будет делать реальный оператор
//! или соседний сервис.
//!
//! Прошивку заливает `cargo xtask test host-target` (release-профиль,
//! bootloader + приложение) и передаёт сюда два значения через окружение:
//! `HOST_TARGET_CHIP` и `HOST_TARGET_PERSIST_ADDR` (адрес региона `PERSIST`,
//! посчитанный по `memory.x`). Через окружение, а не константами в коде: чип
//! подставляется при генерации в одном месте, адрес зависит от чипа.
//!
//! Внешних зависимостей у крейта нет намеренно: `probe-rs` для этого этапа и
//! так обязателен (им же прошивают), и вызвать его как процесс дешевле, чем
//! тянуть в проект его библиотечную версию со своим графом зависимостей.
//!
//! Почему проверка идёт через чтение памяти, а не через defmt-лог: у
//! `probe-rs attach` нет режима «прочитать и выйти», он держит сессию, пока
//! его не убьют. А убитый на Windows `probe-rs` оставляет ST-Link
//! захваченным — следующая команда падает с `reset not supported by WinUSB`
//! до переподключения кабеля (поймано вживую на STM32F3Discovery). Поэтому
//! здесь только одноразовые команды, каждая из которых завершается сама.
//!
//! Когда у платы появится собственный канал связи (USB CDC, UART-протокол,
//! сеть), настоящие сценарии стоит писать поверх него — этот тест останется
//! проверкой того, что устройство живо и его прошивка исполняется.

use std::{env, process::Command, thread, time::Duration};

/// Магия, которую приложение кладёт в начало `PERSIST` (см. `PERSIST_MAGIC` в
/// cross/app/src/main.rs). Совпадение с ней и означает «прошивка дошла до
/// `main` и успела поработать».
const PERSIST_MAGIC: u32 = 0xB007_C0DE;

/// Сколько ждать после сброса, прежде чем читать счётчик. Приложению нужно
/// дойти до `main`, то есть отработать инициализацию HAL; на порядок больше
/// того, что это занимает на практике, но всё ещё мгновение по меркам теста.
const BOOT_TIME: Duration = Duration::from_millis(300);

#[test]
fn firmware_runs_and_counts_boots_across_resets() {
    let chip = required_var("HOST_TARGET_CHIP");
    let persist = required_var("HOST_TARGET_PERSIST_ADDR");

    // Пауза и перед первым чтением, не только после своего сброса: сюда
    // попадают сразу за `cargo flash`, который заканчивается сбросом платы, а
    // на тёплом кеше nextest стартует мгновенно — без задержки чтение обгоняет
    // `main` и тест мигает «прошивка не дошла до main».
    thread::sleep(BOOT_TIME);

    let (magic, first) = read_persist(&chip, &persist);
    assert_eq!(
        magic, PERSIST_MAGIC,
        "в начале PERSIST не магия приложения, а {magic:#010x} — прошивка не дошла до main \
         (или залита не она)"
    );

    reset(&chip);
    thread::sleep(BOOT_TIME);

    let (magic, second) = read_persist(&chip, &persist);
    assert_eq!(
        magic, PERSIST_MAGIC,
        "после сброса магия в PERSIST пропала: {magic:#010x}"
    );
    assert_eq!(
        second,
        first + 1,
        "счётчик запусков не вырос на сбросе: было {first}, стало {second} — либо прошивка \
         не стартовала, либо PERSIST не пережил сброс"
    );
}

/// Первые два слова региона: магия и счётчик запусков.
fn read_persist(chip: &str, address: &str) -> (u32, u32) {
    let output = probe_rs(&["read", "--chip", chip, "b32", address, "2"]);
    let mut words = output.split_whitespace().map(|word| {
        u32::from_str_radix(word, 16)
            .unwrap_or_else(|_| panic!("`probe-rs read` вернул не hex-слово: {word:?}"))
    });
    let magic = words.next().expect("probe-rs read не вернул ни слова");
    let count = words
        .next()
        .expect("probe-rs read вернул только одно слово");
    (magic, count)
}

fn reset(chip: &str) {
    probe_rs(&["reset", "--chip", chip]);
}

fn probe_rs(args: &[&str]) -> String {
    let output = Command::new("probe-rs")
        .args(args)
        .output()
        .expect("не удалось запустить probe-rs — установите его: cargo xtask setup");
    assert!(
        output.status.success(),
        "`probe-rs {}` завершилась с {}:\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn required_var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} не задана: этот этап запускается через `cargo xtask test host-target`, \
             который сам прошивает плату и передаёт сюда чип и адрес PERSIST"
        )
    })
}
