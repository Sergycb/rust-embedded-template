//! Проверка подписи нового образа до обмена разделов.

use core::cell::RefCell;

use defmt::info;
use domain::ota::boot::{ScreenFailure, Screening, screen_update};
use embassy_boot_stm32::BootLoaderConfig;
{%- if boot_partitioned == "true" %}
use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- endif %}
{%- if dfu_bank != "true" %}
use embassy_stm32::flash::{Blocking, Flash};
{%- endif %}
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
{%- if boot_partitioned == "true" %}
use flash_page::ErasePage;
{%- endif %}
{%- if ota_store == "external" %}

use crate::external::ExternalFlash;
{%- elsif dfu_bank == "true" %}

use crate::{Bank1, Bank2{% if state_paged == "true" %}, FLASH_BYTES, STATE_ERASE_SIZE{% endif %}};
{%- elsif boot_partitioned == "true" %}

use crate::{FLASH_BYTES{% if state_paged == "true" %}, STATE_ERASE_SIZE{% endif %}{% if swap_paged == "true" %}, SWAP_PAGE_SIZE{% endif %}};
{%- endif %}

/// Проверяет новый образ в `DFU`, если обмен запрошен, и возвращает флеш
/// дальше — в обмен и прыжок.
///
/// Проверка подписи живёт здесь, а не в приложении: ключ зашит в bootloader,
/// который обновлением не перезаписывается, и проверяется именно то, что
/// сейчас станет `ACTIVE`, — ни ошибка в OTA-коде приложения, ни запись в
/// `DFU` в обход него не приведут к запуску неподписанного образа. Решает
/// `domain::ota::boot::screen_update` (там же — почему проверяется только новый
/// запрос, а откат нет); отказ снимает запрос на обмен, и загружается прежний
/// образ. О неудаче отправитель узнаёт по тому, что версия после перезагрузки
/// не сменилась.
///
/// Причина отказа печатается строкой на вариант, а не через
/// `defmt::Debug2Format`: тот тянет `core::fmt` — несколько килобайт в зону
/// bootloader'а, где их и так впритык.
{%- if ota_store == "external" %}
///
/// Третье в ответе — можно ли обменивать разделы. Нельзя, если внешняя флеш
/// не ответила: образ не проверен, и обмен без проверки запустил бы
/// неподписанный — даже если на следующем шаге флеш вдруг ответит. Тогда
/// загружается текущий образ, запрос на обмен остаётся до следующего сброса.
pub(crate) fn screen(
    flash: Flash<'static, Blocking>,
    external: ExternalFlash,
) -> (Flash<'static, Blocking>, ExternalFlash, bool) {
    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    let external = Mutex::<NoopRawMutex, _>::new(RefCell::new(external));
    let swap_allowed = {
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&flash, &external, &external);
        let capacity = config.active.size();
        if let Ok(screening) = screen_update(
            &mut config.dfu,
            &mut config.state,
            capacity,
            &OTA_PUBLIC_KEY_IN_IMAGE,
        ) {
            report(screening);
            true
        } else {
            defmt::warn!(
                "boot: внешняя флеш не отвечает — новый образ не проверен, обмен пропущен"
            );
            false
        }
    };
    (
        flash.into_inner().into_inner(),
        external.into_inner().into_inner(),
        swap_allowed,
    )
}
{%- elsif dfu_bank == "true" %}
pub(crate) fn screen(flash: Bank1, dfu: Bank2) -> (Bank1, Bank2) {
    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    let dfu = Mutex::<NoopRawMutex, _>::new(RefCell::new(dfu));
    {
{%- if state_paged == "true" %}
        let state = Mutex::<NoopRawMutex, _>::new(RefCell::new(
            ErasePage::<_, STATE_ERASE_SIZE>::new(BlockingPartition::new(&flash, 0, FLASH_BYTES)),
        ));
{%- endif %}
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&flash, &dfu, {% if state_paged == "true" %}&state{% else %}&flash{% endif %});
        let capacity = config.active.size();
        let Ok(screening) = screen_update(
            &mut config.dfu,
            &mut config.state,
            capacity,
            &OTA_PUBLIC_KEY_IN_IMAGE,
        ) else {
            // Флеш не читается — дальше обмен всё равно упал бы; паника хотя
            // бы оставляет причину в PANIC (`cargo xtask panic-info`).
            panic!("boot: проверка нового образа: ошибка флеша");
        };
        report(screening);
    }
    (
        flash.into_inner().into_inner(),
        dfu.into_inner().into_inner(),
    )
}
{%- else %}
pub(crate) fn screen(flash: Flash<'static, Blocking>) -> Flash<'static, Blocking> {
    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    {
{%- if state_paged == "true" %}
        let state = Mutex::<NoopRawMutex, _>::new(RefCell::new(
            ErasePage::<_, STATE_ERASE_SIZE>::new(BlockingPartition::new(&flash, 0, FLASH_BYTES)),
        ));
{%- endif %}
{%- if swap_paged == "true" %}
        // `ACTIVE` и `DFU` — со страницей обмена, как в `main` (`SWAP_PAGE_SIZE`).
        let swap = Mutex::<NoopRawMutex, _>::new(RefCell::new(
            ErasePage::<_, SWAP_PAGE_SIZE>::new(BlockingPartition::new(&flash, 0, FLASH_BYTES)),
        ));
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&swap, &swap, {% if state_paged == "true" %}&state{% else %}&flash{% endif %});
{%- else %}
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, {% if state_paged == "true" %}&state{% else %}&flash{% endif %});
{%- endif %}
        let capacity = config.active.size();
        let Ok(screening) = screen_update(
            &mut config.dfu,
            &mut config.state,
            capacity,
            &OTA_PUBLIC_KEY_IN_IMAGE,
        ) else {
            // Флеш не читается — дальше обмен всё равно упал бы; паника хотя
            // бы оставляет причину в PANIC (`cargo xtask panic-info`).
            panic!("boot: проверка нового образа: ошибка флеша");
        };
        report(screening);
    }
    flash.into_inner().into_inner()
}
{%- endif %}

/// Исход проверки — в лог.
fn report(screening: Screening) {
    match screening {
        Screening::NothingToCheck => {}
        Screening::Approved => info!("boot: подпись нового образа сошлась"),
        Screening::Rejected(why) => defmt::warn!(
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
