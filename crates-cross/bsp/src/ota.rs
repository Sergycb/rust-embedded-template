#![doc = include_str!("../../../docs/modules/bsp-ota.md")]

use embassy_boot::FirmwareUpdaterConfig;
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use crate::FlashMutex;

/// Раздел поверх общего `Flash` чипа — то, из чего собирается адаптер.
///
/// Тот же цельный `Flash`, что и в `crates-cross/boot`: он сам знает реальные
/// границы секторов чипа, в том числе неравномерные (F4/F7/H7), а банковые
/// регионы у каждого семейства называются по-своему.
pub type Partition = BlockingPartition<'static, NoopRawMutex, Flash<'static, Blocking>>;
{%- if signed == "true" %}

/// Адаптер обновления этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным, а generic-адаптер
/// приходит из `adapters`.
pub type Ota = adapters::ota::Signed<Partition, Partition>;

// `pub const FW_VERSION: u32` — версия проекта из Cargo.toml, свёрнутая
// `domain::firmware::pack`. Числа подставляет build.rs: cargo отдаёт версию
// строкой, а в образ нужно число.
include!(concat!(env!("OUT_DIR"), "/fw-version.rs"));

/// Версия этого образа, видимая снаружи по имени символа.
///
/// Существует ради одной вещи: `cargo xtask build` находит её в ELF и
/// дописывает те же четыре байта в хвост `app.bin`. Так у версии остаётся один
/// источник — то, что скомпилировано в прошивку, — и хост не может приписать
/// образу чужой номер, прочитав его из другого места.
///
/// `#[used]` и `no_mangle` вдвоём: первое просит оставить статик, который код
/// не читает, второе — сохранить имя, по которому его ищет xtask.
///
/// Строго говоря, `#[used]` обязывает только компилятор, а не линкер: с
/// `--gc-sections` тот формально волен выбросить символ (rust-lang/rust#47384;
/// штатное лекарство — `KEEP` в линкерном скрипте). На практике LLVM
/// транслирует `llvm.used` в `SHF_GNU_RETAIN`, и в release-сборке символ на
/// месте — проверено `llvm-nm` на собранном ELF. Провал в любом случае будет
/// громким: `cargo xtask build` не найдёт символ и остановит сборку.
#[used]
#[unsafe(no_mangle)]
pub static FW_VERSION_IN_IMAGE: u32 = FW_VERSION;

/// Открытый ключ, которым проверяется подпись образа.
///
/// Не константа в исходнике, а файл `ota-public-key.bin` в корне проекта: его
/// создаёт `cargo xtask build` вместе с закрытым и приносит сюда через
/// `build.rs` (см. `OUT_DIR`). Нули означают «ключ ещё не создан» (файла нет),
/// и `domain::update::apply_signed` отказывает, не доходя до проверки.
pub const PUBLIC_KEY: [u8; 32] = *include_bytes!(concat!(env!("OUT_DIR"), "/ota-public-key.bin"));
{%- else %}

/// Адаптер обновления этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным, а generic-адаптер
/// приходит из `adapters`.
pub type Ota = adapters::ota::Updater<Partition, Partition>;
{%- endif %}

/// Собирает адаптер из разделов `DFU` и `BOOTLOADER_STATE`, найденных по
/// символам `memory.x`, — единственное здесь, что привязано к раскладке чипа.
pub(crate) fn new(flash: &'static FlashMutex) -> Ota {
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
{%- if signed == "true" %}
    let updater = adapters::ota::Updater::new(config.dfu, config.state, max_image_len());
    adapters::ota::Signed::new(updater, FW_VERSION, PUBLIC_KEY)
{%- else %}
    adapters::ota::Updater::new(config.dfu, config.state, max_image_len())
{%- endif %}
}

unsafe extern "C" {
    /// Границы раздела `ACTIVE` — те же символы, по которым разделы находит
    /// `FirmwareUpdaterConfig::from_linkerfile_blocking`.
    static __bootloader_active_start: u32;
    static __bootloader_active_end: u32;
}

/// Самый длинный образ, который вообще доедет до устройства.
///
/// Это размер `ACTIVE`, а НЕ `DFU`, хотя принимается образ в `DFU`. Раздел
/// обновления по построению больше активного на одну-две страницы (этого
/// требует `assert_partitions` в `embassy-boot`: обмену нужна запасная
/// страница), а обмен копирует ровно `ACTIVE`. Образ размером между ними
/// прошёл бы и проверку длины, и подпись, и пометку к обмену — а в `ACTIVE`
/// приехал бы обрезанным: устройство отработало бы полный цикл обновления с
/// перезагрузкой и молча откатилось.
fn max_image_len() -> u32 {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные значения;
    // берётся их адрес, а не содержимое.
    let start = &raw const __bootloader_active_start as u32;
    let end = &raw const __bootloader_active_end as u32;
    end.saturating_sub(start)
}
