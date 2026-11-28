//! Проверка подписи нового образа до обмена разделов.

use defmt::{info, warn};
use domain::ota::boot::{ScreenFailure, Screening, screen_update};
use embassy_boot_stm32::BootLoaderConfig;
use embedded_storage::nor_flash::NorFlash;

/// Открытый ключ, которым проверяется подпись нового образа.
///
/// Не константа в исходнике, а файл `ota-public-key.bin` в корне проекта: его
/// создаёт `cargo xtask build` вместе с закрытым и приносит сюда `build.rs`
/// через `OUT_DIR`. Нули означают «ключ ещё не создан» (файла нет), и
/// `screen_update` отвергает с ними любой образ.
///
/// Статик с именем, а не `const`: `cargo xtask build` находит его в ELF
/// bootloader'а и сверяет с ключом, которым подписывает образ. `#[used]` и
/// `no_mangle` вдвоём: первое просит оставить статик, второе — сохранить имя.
#[used]
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")] // имя символа нужно xtask, см. выше
#[unsafe(no_mangle)]
pub static OTA_PUBLIC_KEY_IN_IMAGE: [u8; 32] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/ota-public-key.bin"));

/// Проверяет новый образ в `DFU`, если обмен запрошен, и возвращает конфиг
/// разделов дальше — в обмен и прыжок.
///
/// Проверка подписи живёт здесь, а не в приложении (docs/ota.md, «Подпись
/// OTA-образа»). Решает `domain::ota::boot::screen_update` (там же — почему
/// проверяется только новый запрос, а откат нет); отказ снимает запрос на
/// обмен, и загружается прежний образ.
///
/// Причина отказа печатается строкой на вариант, а не через
/// `defmt::Debug2Format`: тот тянет `core::fmt` — несколько килобайт в зону
/// bootloader'а, где их и так впритык.
///
/// Разделы приходят готовыми (`BootLoaderConfig`): их границы — символы
/// `memory.x` этого бинарника (`crate::layout`), а не линкерные символы
/// `embassy-boot`, потому что мьютексы у них общие с приложением
/// (`ThreadModeRawMutex`, а не неблокирующий).
{%- if ota_store == "external" %}
///
/// Второе в ответе — можно ли обменивать разделы. Нельзя, если внешняя флеш
/// не ответила: образ не проверен, и обмен без проверки запустил бы
/// неподписанный — даже если на следующем шаге флеш вдруг ответит. Тогда
/// загружается текущий образ, и запрос на обмен до следующего сброса не
/// доживает: прежнее приложение в стартовом действии узла OTA (`begin`) зовёт
/// `mark_booted`, а оно снимает магию `Swap`. Разовая ошибка флеши здесь —
/// потерянное обновление: хост узнаёт об этом по прежней версии и повторяет
/// (docs/ota.md, «Bootloader и внешняя флеш»). Кроме отказа на журнале после прочитанной магии `Swap`
/// (`Screening::JournalUnreadable`): обмен, может быть, идёт, и `report`
/// паникует, а не отдаёт `ACTIVE` прыжку.
pub(crate) fn screen<ACTIVE, DFU, STATE>(
    mut config: BootLoaderConfig<ACTIVE, DFU, STATE>,
) -> (BootLoaderConfig<ACTIVE, DFU, STATE>, bool)
where
    ACTIVE: NorFlash,
    DFU: NorFlash,
    STATE: NorFlash,
{
    let swap_allowed = if let Ok(screening) = screen_update(
        &mut config.dfu,
        &mut config.state,
        crate::layout::active().1,
        &OTA_PUBLIC_KEY_IN_IMAGE,
    ) {
        report(screening);
        true
    } else {
        warn!("boot: внешняя флеш не отвечает — новый образ не проверен, обмен пропущен");
        false
    };
    (config, swap_allowed)
}
{%- else %}
pub(crate) fn screen<ACTIVE, DFU, STATE>(
    mut config: BootLoaderConfig<ACTIVE, DFU, STATE>,
) -> BootLoaderConfig<ACTIVE, DFU, STATE>
where
    ACTIVE: NorFlash,
    DFU: NorFlash,
    STATE: NorFlash,
{
    let Ok(screening) = screen_update(
        &mut config.dfu,
        &mut config.state,
        crate::layout::active().1,
        &OTA_PUBLIC_KEY_IN_IMAGE,
    ) else {
        // Флеш не читается — дальше обмен всё равно упал бы: паника, а что
        // за ней — решает паникёр bootloader'а (`boot/src/main.rs`).
        panic!("boot: проверка нового образа: ошибка флеша");
    };
    report(screening);
    config
}
{%- endif %}

/// Исход проверки — в лог; журнал `STATE` не прочитан — паника.
fn report(screening: Screening) {
    match screening {
        // Обмен, может быть, идёт: прыжок в `ACTIVE` мимо `prepare` попал бы в
        // смесь страниц. Паника — сброс и повтор с журналом (паникёр —
        // `boot/src/main.rs`), как и на любой ошибке флеша в строгом `prepare`.
        Screening::JournalUnreadable => panic!("boot: журнал STATE не читается"),
        Screening::NothingToCheck => {}
        Screening::Approved => info!("boot: подпись нового образа сошлась"),
        Screening::Rejected(why) => warn!(
            "boot: новый образ отвергнут ({=str}), загружается прежний",
            match why {
                ScreenFailure::NoRecord => "нет записи о подписи",
                ScreenFailure::Length => "негодная длина",
                ScreenFailure::NoPublicKey => "ключ не подставлен",
                ScreenFailure::Signature => "подпись не сошлась",
            }
        ),
    }
}
