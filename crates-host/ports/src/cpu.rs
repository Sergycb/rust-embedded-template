//! Порт чипа: ассоциированные типы его периферии и единственный `split(self)`.
//! Реализация — `crates-cross/cpu`; `bsp` видит только этот трейт.

use crate::Reboot;

/// Чип и его распиновка: периферия, разложенная по рычагам.
///
/// Трейт, а не конкретный тип, потому что `bsp` — плата — не знает ни одной
/// периферии чипа и не зависит от `embassy-stm32`; прошивка собирает её над
/// этим портом. Реализация одна (`crates-cross/cpu`), но без трейта `bsp` не
/// выразить: `domain` и `bsp` не видят друг друга, а типы полей `Board` —
/// периферии чипа.
pub trait Cpu {
    /// Сведения о чипе: имя, заводской номер, объём flash, частота `sys`.
    type CpuInfo: core::fmt::Debug;
    /// Тип сырого мьютекса: `app`/`boot` задают его одним alias.
    type Mutex;
    /// Сырой драйвер внутреннего флеша — без разделов.
    type Flash;
{%- if external_flash == "true" %}
    /// Сырая шина внешней флеши — без разделов.
    type ExtFlashBus;
{%- endif %}
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- assign parts = f[4] | split: "_" %}
{%- if f[0] == "uart" %}
    /// UART `{{ f[2] }}` — поток байтов.
{%- elsif f[0] == "can" %}
    /// Шина CAN `{{ f[2] }}`.
{%- else %}
    /// Драйвер USB-блока `{{ f[2] }}`.
{%- endif %}
    type {% for part in parts %}{{ part | capitalize }}{% endfor %};
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
    /// HAL-драйвер Ethernet: трейт драйвера связывает `bsp`, здесь он не назван.
    type Ethernet;
{%- endif %}
    /// Система: перезагрузка МК и причина прошлого сброса.
    type System: Reboot;
{%- if graph == "true" %}
    /// Сторож платы — заряженный, но ещё не запущенный.
    type Watchdog;
{%- endif %}

    /// Наименьший реальный таймаут сторожа, мкс: `HW_TIMEOUT_US` из `cpu::wdg`
    /// при LSI на верхнем пределе семейства.
    ///
    /// С ним, а не с номиналом, сверяет таймауты узлов `const`-assert цепочки в
    /// `app/src/graph.rs`. Без сторожа — ноль: цепочки нет и сравнивать не с чем.
    const WATCHDOG_MIN_TIMEOUT_US: u64;

    /// Заводской номер экземпляра чипа — 96 бит, не меняется.
    ///
    /// Отдельным методом, а не полем `CpuInfo`: плате он нужен до `split`
    /// (серийные номера USB-устройств), а поля ассоциированного типа она не
    /// видит.
    fn uid(&self) -> [u8; 12];

    /// Потребляет чип и отдаёт все его периферии одним кортежем.
    ///
    /// Порядок — как в полях `bsp::Board`: сведения о чипе, сырые флеши,
    /// периферия транспортов в порядке `links`, Ethernet, система, сторож,
    /// затравка стека.
    #[must_use]
    #[allow(
        clippy::type_complexity,
        reason = "кортеж — это и есть весь набор периферии чипа: по типу на рычаг"
    )]
    fn split(
        self,
    ) -> (
        // По рычагу на строку: их набор зависит от генерации.
        Self::CpuInfo,
        Self::Flash,
{%- if external_flash == "true" %}
        Self::ExtFlashBus,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- assign parts = f[4] | split: "_" %}
        Self::{% for part in parts %}{{ part | capitalize }}{% endfor %},
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
        Self::Ethernet,
{%- endif %}
        Self::System,
{%- if graph == "true" %}
        Self::Watchdog,
{%- endif %}
{%- if has_net == "true" %}
        u64, // затравка случайных чисел стека (порты, номера последовательностей TCP)
{%- endif %}
    );
}
