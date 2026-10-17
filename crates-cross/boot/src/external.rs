//! `STATE` и `DFU` на внешней флеши: драйвер, страница обмена и решение,
//! менять ли разделы местами.

use domain::ota::boot::{BootPlan, boot_plan};
use embassy_boot_stm32::{BootLoader, BootLoaderConfig, State};
use embedded_storage::nor_flash::NorFlash;

/// Страница обмена, под которую посчитана раскладка (`memory.x`): `ACTIVE`
/// кратен ей. Обмен идёт страницами `max(ACTIVE::ERASE_SIZE,
/// DFU::ERASE_SIZE)`, и драйвер с более крупным стиранием потребовал бы
/// `ACTIVE`, кратного уже его странице, — `assert_partitions` паниковал бы на
/// старте. Поэтому стирание драйвера обязано эту страницу делить. Число —
/// переменная среды `EXT_PAGE_SIZE` из `[env]` `crates-cross/.cargo/config.toml`,
/// та же, что читает приложение (`bsp::external`).
const EXT_PAGE_SIZE: usize = adapters::flash::size_from_env(option_env!("EXT_PAGE_SIZE"), 0);

/// Драйвер внешней флеши, на которой лежат `STATE` и `DFU`, — замените на
/// свой: любой блокирующий `embedded_storage::nor_flash::NorFlash`. Та же
/// микросхема, что у приложения (`bsp::external`), но драйвер блокирующий:
/// bootloader не поднимает исполнитель.
pub(crate) type ExternalFlash = adapters::flash::Unconnected;

const _: () = assert!(
    EXT_PAGE_SIZE != 0 && EXT_PAGE_SIZE.is_multiple_of(<ExternalFlash as NorFlash>::ERASE_SIZE),
    "стирание драйвера внешней флеши не делит страницу обмена (EXT_PAGE_SIZE): \
     перегенерируйте раскладку или возьмите стирание мельче (сектор 4 KiB)"
);

/// Журнал `STATE` в раскладке посчитан на слово не шире 32 байт — столько же
/// держит буфер адаптера в приложении (`adapters::ota::MAX_WRITE_SIZE`).
const _: () = assert!(
    <ExternalFlash as NorFlash>::WRITE_SIZE <= adapters::ota::MAX_WRITE_SIZE,
    "слово записи драйвера внешней флеши шире 32 байт"
);

/// Буфер обмена: кратен слову записи и внутреннего флеша (`ACTIVE`), и
/// внешнего (`DFU`), и не меньше слова `STATE` — требования `prepare_boot`.
/// Оба слова — степени двойки, так что общее кратное — большее из них.
const BUFFER_SIZE: usize = if <ExternalFlash as NorFlash>::WRITE_SIZE > {{write_size}} {
    <ExternalFlash as NorFlash>::WRITE_SIZE
} else {
    {{write_size}}
};

const _: () = assert!(
    BUFFER_SIZE.is_multiple_of(embassy_stm32::flash::WRITE_SIZE),
    "буфер обмена не кратен слову записи внутреннего флеша (embassy_stm32::flash::WRITE_SIZE): \
     проверьте write_size из генерации"
);

/// Подготовка к загрузке. Решает `domain::ota::boot::boot_plan` (там же почему
/// и host-тест с прерванным обменом): `Prepare` — строгий
/// `BootLoader::prepare`, ошибка которого — паника, сброс и продолжение обмена
/// по журналу, а не прыжок в наполовину переписанный `ACTIVE`; `BootCurrent` —
/// `STATE` не читается (внешняя флеш не отвечает), был ли начат обмен, узнать
/// нельзя, и загружается текущий образ без обмена.
pub(crate) fn prepare<ACTIVE: NorFlash, DFU: NorFlash, STATE: NorFlash>(
    mut config: BootLoaderConfig<ACTIVE, DFU, STATE>,
) -> BootLoader {
    match boot_plan(&mut config.state) {
        BootPlan::Prepare => BootLoader::prepare::<_, _, _, BUFFER_SIZE>(config),
        BootPlan::BootCurrent => {
            defmt::warn!("boot: внешняя флеш не отвечает — загружается текущий образ");
            BootLoader { state: State::Boot }
        }
    }
}
