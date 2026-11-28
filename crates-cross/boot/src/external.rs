//! `STATE` и `DFU` на внешней флеши: состояние драйвера и страница обмена.
//!
//! Драйвер и его шину держит `cpu::external` — тот же, что у приложения;
//! блокирующий, потому что bootloader не поднимает исполнитель. `cpu::init`
//! драйвер только собирает, микросхему опознаёт `cpu::external::init` (его
//! зовёт `main` до проверки подписи). Здесь — забор опознанного состояния из
//! руки чипа и строгий `prepare` над ним.

// Страница обмена, под которую посчитана раскладка (`memory.x`): `ACTIVE`
// кратен ей. Обмен идёт страницами `max(ACTIVE::ERASE_SIZE,
// DFU::ERASE_SIZE)`, и драйвер с более крупным стиранием потребовал бы
// `ACTIVE`, кратного уже его странице, — `assert_partitions` паниковал бы на
// старте. Поэтому стирание драйвера обязано эту страницу делить. Число — то
// же, что у приложения (`cpu::external`).
use cpu::external::{EXT_PAGE_SIZE, ExtFlashState};
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embedded_storage::nor_flash::NorFlash;

use cpu::flash::WRITE_SIZE;

const _: () = assert!(
    EXT_PAGE_SIZE.is_multiple_of(<ExtFlashState as NorFlash>::ERASE_SIZE),
    "стирание драйвера внешней флеши не делит страницу обмена (EXT_PAGE_SIZE): \
     перегенерируйте раскладку или возьмите стирание мельче (сектор 4 KiB)"
);

/// Журнал `STATE` в раскладке посчитан на слово не шире 32 байт — столько же
/// держит буфер адаптера в приложении (`ports::MAX_WRITE_SIZE`).
const _: () = assert!(
    <ExtFlashState as NorFlash>::WRITE_SIZE <= ports::MAX_WRITE_SIZE,
    "слово записи драйвера внешней флеши шире 32 байт"
);

/// Буфер обмена: кратен слову записи и внутреннего флеша (`ACTIVE`), и
/// внешнего (`DFU`), и не меньше слова `STATE` — требования `prepare_boot`.
/// Оба слова — степени двойки, так что общее кратное — большее из них.
const BUFFER_SIZE: usize = if <ExtFlashState as NorFlash>::WRITE_SIZE > {{swap_buffer}} {
    <ExtFlashState as NorFlash>::WRITE_SIZE
} else {
    {{swap_buffer}}
};

const _: () = assert!(
    BUFFER_SIZE.is_multiple_of(WRITE_SIZE),
    "буфер обмена не кратен слову записи внутреннего флеша (cpu::flash::WRITE_SIZE): \
     проверьте swap_buffer из генерации"
);

/// Забирает опознанный драйвер из руки чипа. После этого статик чипа держит
/// [`ExtFlashState::Broken`] — приложение его не увидит: у него своя копия
/// статика и свой `init`.
pub(crate) fn take(handle: cpu::external::ExtFlash) -> ExtFlashState {
    handle
        .lock
        .lock(|state| core::mem::replace(&mut *state.borrow_mut(), ExtFlashState::Broken))
}

/// Строгий `BootLoader::prepare`: зовётся, когда проверка нового образа
/// (`signature::screen`) прочитала `STATE`, — обмен запрошен, идёт или его
/// нет, и ошибка флеши посреди него кончается паникой и сбросом, а не прыжком
/// в наполовину переписанный `ACTIVE` (docs/ota.md, «Bootloader и внешняя
/// флеш»). `STATE` второй раз не читается: удачная проверка уже показала, что
/// он читается, а повторное чтение, отказав, отправило бы bootloader в `ACTIVE`
/// посреди обмена.
pub(crate) fn prepare<ACTIVE: NorFlash, DFU: NorFlash, STATE: NorFlash>(
    config: BootLoaderConfig<ACTIVE, DFU, STATE>,
) -> BootLoader {
    BootLoader::prepare::<_, _, _, BUFFER_SIZE>(config)
}
