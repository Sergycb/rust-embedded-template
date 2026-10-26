//! Причина последнего сброса МК.
//!
//! Читает её `bsp` (флаги сброса `RCC`, один раз — в `Board::new`), а отдаёт
//! домен: узлы, отвечающие на «кто ты», кладут её в сведения об устройстве.

/// Почему МК сбросился в последний раз — по флагам `RCC`.
///
/// Флагов бывает несколько сразу (внутренний сброс выставляет и флаг вывода
/// `NRST`), поэтому причина — одна, самая конкретная: порядок вариантов здесь
/// и есть порядок, в котором `bsp` их проверяет, кроме [`Unknown`](Self::Unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ResetCause {
    /// Выход из режима низкого потребления сбросом (`Standby`/`Stop` с
    /// запретом входа).
    LowPower,
    /// Оконный сторож (`WWDG`).
    WindowWatchdog,
    /// Независимый сторож (`IWDG`).
    IndependentWatchdog,
    /// Программный сброс (`SYSRESETREQ`): паника в release, `Reboot`.
    Software,
    /// Перезагрузка option bytes.
    OptionBytes,
    /// Включение питания или его провал (POR/PDR/BOR).
    PowerOn,
    /// Внешний сброс выводом `NRST` (кнопка, отладчик).
    Pin,
    /// Ни один из известных флагов не выставлен.
    Unknown,
}

impl ResetCause {
    /// Имя причины в `snake_case` — для JSON и консоли.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LowPower => "low_power",
            Self::WindowWatchdog => "window_watchdog",
            Self::IndependentWatchdog => "independent_watchdog",
            Self::Software => "software",
            Self::OptionBytes => "option_bytes",
            Self::PowerOn => "power_on",
            Self::Pin => "pin",
            Self::Unknown => "unknown",
        }
    }
}
