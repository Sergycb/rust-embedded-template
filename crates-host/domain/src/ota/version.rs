//! Версия прошивки как одно число и правило, по которому обновление
//! принимается.
//!
//! Здесь, а не в `bsp`: «стоит ли принимать этот образ» — правило продукта, а
//! не свойство флеша. `bsp` знает, откуда взять четыре байта, `domain` — что
//! они значат. Заодно правило становится проверяемым на хосте: подобрать
//! граничные пары чисел в host-тесте несравнимо дешевле, чем воспроизводить их
//! на плате.
//!
//! Пользуется этим `update::apply` — применение образа узлом OTA. Образы
//! всегда подписаны: без подписи версию в образе правил бы кто угодно, и
//! проверка создавала бы видимость защиты. Модуль — часть `domain::ota`: в
//! проекте без OTA его, как и всего `domain::ota`, нет.
//!
//! Зачем вообще: подпись доказывает, КТО прислал образ, но не то, что он
//! новее. Без сравнения версий злоумышленнику достаточно взять вашу же
//! прошлую прошивку — честно подписанную, с уже известной уязвимостью — и
//! скормить её устройству. Атака называется откатом, и от неё не спасает ни
//! ключ, ни хеш.

/// Версия, свёрнутая в 32 бита: `major` и `minor` по байту, `patch` — два.
///
/// Такой формат, потому что версия едет ВНУТРИ образа, в секции фиксированного
/// размера: строку туда не положить, а сравнивать всё равно нужно числа.
/// Границы полей выбраны под semver реальных прошивок — мажор и минор редко
/// уходят за две сотни, а патч у долгоживущего устройства вполне доберётся до
/// тысяч.
///
/// Функция `const`: значение подставляется на этапе компиляции, в статик,
/// который линкер кладёт в образ.
#[must_use]
pub const fn pack(major: u8, minor: u8, patch: u16) -> u32 {
    ((major as u32) << 24) | ((minor as u32) << 16) | patch as u32
}

/// [`pack`] из semver-строки `major.minor.patch` — как `version` в Cargo.toml.
///
/// Пререлизный суффис не принимается: semver считает `1.2.3-rc1` младше
/// `1.2.3`, а уместить это в четыре байта нечем — ошибка компиляции, а не
/// прошивка, молча принимающая откат. Компонент, не влезший в свое поле
/// (`major`/`minor` до 255, `patch` до 65535), — тоже.
///
/// # Panics
///
/// В `const`-контексте — ошибка компиляции: строка не `major.minor.patch`,
/// компонент не число, компонент шире своего поля, пререлизный суффикс.
#[must_use]
pub const fn pack_str(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let (major, next) = component(bytes, 0, u8::MAX as u32);
    assert!(
        next < bytes.len() && bytes[next] == b'.',
        "pack_str: ожидается major.minor.patch"
    );
    let (minor, next) = component(bytes, next + 1, u8::MAX as u32);
    assert!(
        next < bytes.len() && bytes[next] == b'.',
        "pack_str: ожидается major.minor.patch"
    );
    let (patch, next) = component(bytes, next + 1, u16::MAX as u32);
    assert!(
        next == bytes.len(),
        "pack_str: пререлизный суффикс версии не поддерживается"
    );
    // Границы полей проверены `component` — усечение недостижимо.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "границы полей проверены в component"
    )]
    pack(major as u8, minor as u8, patch as u16)
}

/// Число из десятичных цифр `bytes[start..]` до конца или `.`; что после —
/// индекс разделителя (или длины).
const fn component(bytes: &[u8], start: usize, max: u32) -> (u32, usize) {
    let mut value = 0_u32;
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'.' {
        let digit = bytes[i].wrapping_sub(b'0');
        assert!(digit <= 9, "pack_str: компонент версии — не число");
        value = value * 10 + digit as u32;
        assert!(
            value <= max,
            "pack_str: компонент версии не влезает в свое поле"
        );
        i += 1;
    }
    assert!(i > start, "pack_str: пустой компонент версии");
    (value, i)
}

/// Принимать ли образ версии `incoming` устройству, на котором работает
/// `current`.
///
/// Строго больше, а не «больше либо равно», и это не придирка: повторная
/// заливка той же версии — типичный вид атаки отката, когда злоумышленник
/// подсовывает другую сборку с тем же номером. Хотите переустановить —
/// поднимите патч; это дешевле, чем оставлять дыру ради удобства.
///
/// Порядок сравнения — обычный числовой, и он совпадает с semver ровно
/// потому, что [`pack`] раскладывает поля по старшинству.
#[must_use]
pub const fn is_upgrade(current: u32, incoming: u32) -> bool {
    incoming > current
}

#[cfg(test)]
mod tests {
    use super::{is_upgrade, pack, pack_str};

    /// Раскладка полей закреплена конкретными числами, а не только взаимным
    /// порядком: те же литералы стоят в тесте `xtask`, который про этот крейт
    /// ничего не знает. Проверяй тесты только порядок — смена раскладки на
    /// 16/8/8 прошла бы мимо обоих, а `version::pack_str` (константа `IMAGE`
    /// каждого бинарника) от неё зависит по-настоящему: она разбирает поля.
    #[test]
    fn packs_into_the_agreed_bit_layout() {
        assert_eq!(pack(1, 2, 3), 0x0102_0003);
        assert_eq!(pack(0, 1, 0), 0x0001_0000);
        assert_eq!(pack(255, 255, 65535), 0xFFFF_FFFF);
    }

    #[test]
    fn packs_fields_in_order_of_significance() {
        assert!(pack(1, 0, 0) > pack(0, 99, 999));
        assert!(pack(0, 2, 0) > pack(0, 1, 999));
        assert!(pack(0, 0, 2) > pack(0, 0, 1));
    }

    /// Та самая пара, ради которой всё затевалось: старый образ моложе не
    /// становится.
    #[test]
    fn refuses_an_older_image() {
        assert!(!is_upgrade(pack(1, 2, 0), pack(1, 1, 9)));
        assert!(!is_upgrade(pack(2, 0, 0), pack(1, 9, 9)));
    }

    /// Та же версия — тоже отказ: см. рассуждение у `is_upgrade`.
    #[test]
    fn refuses_the_same_version() {
        assert!(!is_upgrade(pack(1, 2, 3), pack(1, 2, 3)));
    }

    /// `pack_str` — тот же `pack` из строки Cargo.toml, включая отказ от
    /// пререлизного суффикса и переполнение полей.
    #[test]
    fn pack_str_parses_semver_text() {
        assert_eq!(pack_str("1.2.3"), pack(1, 2, 3));
        assert_eq!(pack_str("0.1.0"), 0x0001_0000);
        assert_eq!(pack_str("255.255.65535"), u32::MAX);
    }

    #[test]
    #[should_panic(expected = "не число")]
    fn pack_str_refuses_a_pre_release_suffix() {
        let _packed = pack_str("1.2.3-rc1");
    }

    #[test]
    #[should_panic(expected = "не влезает")]
    fn pack_str_refuses_an_overflowing_component() {
        let _packed = pack_str("1.2.65536");
    }

    #[test]
    fn accepts_a_newer_image() {
        assert!(is_upgrade(pack(1, 2, 3), pack(1, 2, 4)));
        assert!(is_upgrade(pack(1, 2, 3), pack(2, 0, 0)));
    }

    /// Нулевая текущая версия — это `0.0.0`, то есть проект, где версию ещё не
    /// проставили. Обновление на неё не встаёт, а с неё — встаёт любое: иначе
    /// свежий проект не смог бы принять первое же обновление.
    #[test]
    fn a_zero_current_version_accepts_anything_above_it() {
        assert!(is_upgrade(0, pack(0, 0, 1)));
        assert!(!is_upgrade(0, 0));
    }
}
