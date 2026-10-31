//! `STATE` и `DFU` на внешней флеши: драйвер и страница обмена.

// Страница обмена, под которую посчитана раскладка (`memory.x`): `ACTIVE`
// кратен ей. Обмен идёт страницами `max(ACTIVE::ERASE_SIZE,
// DFU::ERASE_SIZE)`, и драйвер с более крупным стиранием потребовал бы
// `ACTIVE`, кратного уже его странице, — `assert_partitions` паниковал бы на
// старте. Поэтому стирание драйвера обязано эту страницу делить. Число — то
// же, что у приложения (`bsp::external`).
use bsp::external::EXT_PAGE_SIZE;
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embedded_storage::nor_flash::NorFlash;

const _: () = assert!(
    EXT_PAGE_SIZE.is_multiple_of(<ExternalFlash as NorFlash>::ERASE_SIZE),
    "стирание драйвера внешней флеши не делит страницу обмена (EXT_PAGE_SIZE): \
     перегенерируйте раскладку или возьмите стирание мельче (сектор 4 KiB)"
);

/// Журнал `STATE` в раскладке посчитан на слово не шире 32 байт — столько же
/// держит буфер адаптера в приложении (`ports::MAX_WRITE_SIZE`).
const _: () = assert!(
    <ExternalFlash as NorFlash>::WRITE_SIZE <= ports::MAX_WRITE_SIZE,
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

/// Драйвер внешней флеши, на которой лежат `STATE` и `DFU`: та же W25Q, что
/// у приложения (`bsp::external`), но блокирующая — bootloader не поднимает
/// исполнитель. Шина — пока заглушка; подключая микросхему, плата меняет её
/// здесь и в `bsp::external` (docs/modules/bsp-external.md).
///
/// Задержки — на `SysTick` ядра: исполнителя и драйвера времени здесь нет.
/// Счётчик `SysTick` включается только на время задержки, и прыжок в
/// приложение застаёт его выключенным.
pub(crate) type ExternalFlash = w25q::device::W25qSync<bsp::stub::FlashBus, cortex_m::delay::Delay>;

/// Собирает драйвер и опознаёт микросхему. Не прошёл `init` или микросхема
/// меньше разделов (`bsp::layout::ext_flash_end`) — строка в лог, и драйвер
/// отказывает в любой операции: `STATE` не читается, и [`prepare`] загружает
/// текущий образ без обмена, а не упирается посреди обмена в конец микросхемы.
///
/// # Panics
///
/// Если периферию ядра уже кто-то взял (`cortex_m::Peripherals::take`): у
/// `SysTick` задержки один владелец, и зовётся `new` один раз.
pub(crate) fn new(sys_hz: u32) -> ExternalFlash {
    let core = cortex_m::Peripherals::take().expect("boot: периферия ядра уже взята");
    let delay = cortex_m::delay::Delay::new(core.SYST, sys_hz);
    let mut flash = ExternalFlash::new(bsp::stub::FlashBus, delay);
    match flash.init() {
        Ok(_) if flash.capacity() >= bsp::layout::ext_flash_end() => flash,
        Ok(_) => {
            defmt::error!(
                "boot: внешняя флеш {=u32} KiB меньше разделов — загружается текущий образ",
                flash.capacity() / 1024
            );
            let (bus, delay) = flash.release();
            ExternalFlash::new(bus, delay)
        }
        Err(err) => {
            defmt::warn!("boot: внешняя флеш не отвечает: {}", err);
            flash
        }
    }
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
