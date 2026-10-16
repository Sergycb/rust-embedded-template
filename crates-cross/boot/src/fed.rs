//! Сторож в release: его таймаут и флеш, который кормит его на обмене.

use core::cell::RefCell;

use embassy_stm32::wdg::IndependentWatchdog;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

/// Таймаут сторожа — то же число из `[env]` crates-cross/.cargo/config.toml,
/// что у приложения (`bsp::wdg::HW_TIMEOUT_US`, там же и почему одно).
pub(crate) const WATCHDOG_TIMEOUT_US: u32 =
    match u32::from_str_radix(env!("WATCHDOG_TIMEOUT_US"), 10) {
        Ok(us) => us,
        Err(_) => panic!("WATCHDOG_TIMEOUT_US в .cargo/config.toml — не число"),
    };

/// Флеш, кормящий сторож перед каждым стиранием и записью.
///
/// Обмен разделов внутри `BootLoader::prepare` — один блокирующий вызов, и на
/// крупных секторах он длится дольше любого таймаута IWDG: кормить снаружи его
/// нечем. Изнутри — можно: всё, что делает обмен, идёт через этот объект.
///
/// Только release и только в проекте с графом. В debug-профиле bootloader'ом
/// пользуются target-тесты (`cargo xtask test target`), а кормить сторож в
/// них некому; без графа — то же самое в самом приложении.
///
/// Зачем сторож здесь вообще: IWDG, запущенный однажды, до сброса не
/// останавливается. Новый образ, зависший раньше тикера графа, сбрасывается
/// им, не подтвердив себя (`mark_booted`), и следующий старт bootloader'а
/// откатывает его — без сторожа плата висела бы до выключения питания.
///
/// Сторож — по ссылке, а не полем: флешей под обменом может быть две
/// (внутренняя и внешняя, `ota=external`), а сторож один.
pub(crate) struct Fed<'w, F> {
    flash: F,
    watchdog: &'w Watchdog,
}

/// Запущенный сторож, общий для всех [`Fed`].
pub(crate) type Watchdog = RefCell<IndependentWatchdog<'static, embassy_stm32::peripherals::{{watchdog_peripheral}}>>;

impl<'w, F> Fed<'w, F> {
    pub(crate) const fn new(flash: F, watchdog: &'w Watchdog) -> Self {
        Self { flash, watchdog }
    }
}

impl<F: ErrorType> ErrorType for Fed<'_, F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for Fed<'_, F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

impl<F: NorFlash> NorFlash for Fed<'_, F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    /// Длинный диапазон — по кускам до границы `ERASE_SIZE`, со сторожем перед
    /// каждым. `embassy-boot` стирает страницу обмена или весь
    /// `BOOTLOADER_STATE` (после отката) одним вызовом, и у внешней флеши со
    /// страницей 4 KiB это десятки команд стирания по 45–400 мс — дольше
    /// таймаута сторожа.
    ///
    /// Резать по самому `ERASE_SIZE` от начала диапазона было бы нельзя: у
    /// внутреннего флеша это крупнейший сектор, а на F4/F7/H7 секторы
    /// неравные, и кусок мог бы кончиться посреди настоящего сектора — отказ
    /// стирания. Граница же кратна `ERASE_SIZE` и потому всегда совпадает с
    /// границей сектора (они кратны своему размеру): у внутреннего флеша
    /// страница обмена и `STATE` в мелких секторах так и остаются одним
    /// куском. Таймаут сторожа должен пережить стирание одного куска — самого
    /// крупного сектора чипа.
    fn erase(&mut self, mut from: u32, to: u32) -> Result<(), Self::Error> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "`ERASE_SIZE` флеша — меньше 4 ГиБ"
        )]
        let page = F::ERASE_SIZE as u32;
        while from < to {
            let end = to.min((from / page + 1) * page);
            self.watchdog.borrow_mut().pet();
            self.flash.erase(from, end)?;
            from = end;
        }
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.watchdog.borrow_mut().pet();
        self.flash.write(offset, bytes)
    }
}
