//! Обновление прошивки: всё, что не зависит от канала доставки.
//!
//! Канал (USB CDC, UART-протокол, сеть, SD-карта) у каждой платы свой, и
//! шаблон его не выбирает. Но вокруг канала есть работа, одинаковая для всех:
//! записать полученные байты в раздел `DFU`, попросить bootloader поменять
//! разделы местами и — самое неочевидное — подтвердить новый образ после
//! того, как он завёлся. Она здесь и живёт.
//!
//! Транспорт остаётся снаружи и сводится к одному вызову в цикле приёма:
//!
//! ```ignore
//! let mut offset = 0;
//! while let Some(chunk) = link.next_chunk().await {
//!     board.ota.write(offset, chunk)?;
//!     offset += chunk.len();
//! }
//! board.ota.mark_updated()?;
//! cortex_m::peripheral::SCB::sys_reset();
//! ```
//!
//! # Про подтверждение: без него обновление живёт один запуск
//!
//! `embassy-boot` меняет разделы местами и ждёт, что новый образ отметится
//! как работоспособный ([`Ota::mark_booted`]). Не отметился — на следующем
//! сбросе bootloader откатит обновление обратно. Это не придирка, а
//! страховка: образ, который не доходит до подтверждения, скорее всего не
//! доходит и до полезной работы, и откат возвращает плату в живое состояние
//! без человека рядом.
//!
//! Отсюда два следствия, о которых легко забыть:
//!
//! * подтверждать надо **из нового образа**, а не в конце процедуры
//!   обновления;
//! * момент подтверждения — это и есть определение «прошивка работает».
//!   В первой строке `main` оно бессмысленно (подтверждён будет любой образ,
//!   который смог стартовать); правильное место — там, где устройство
//!   доказало работоспособность: подняло периферию, ответило на первый
//!   запрос, отработало цикл.
//!
//! Пока образ не подтверждён, [`Ota::write`] отказывает: `embassy-boot` не
//! даёт затирать `DFU`, в котором лежит образ, куда откатываться.

use core::cell::RefCell;

use embassy_boot_stm32::{AlignedBuffer, BlockingFirmwareUpdater, FirmwareUpdaterConfig};
use embassy_stm32::Peri;
use embassy_stm32::flash::{Blocking, Flash, WRITE_SIZE};
use embassy_stm32::peripherals::FLASH;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_storage::nor_flash::NorFlash;

// Оба типа возвращаются методами ниже, поэтому называть их должно быть чем —
// иначе пользователю пришлось бы объявлять прямую зависимость на
// `embassy-boot-stm32` только ради имени в сигнатуре своей функции.
pub use embassy_boot::FirmwareUpdaterError as Error;
pub use embassy_boot_stm32::State;

/// Доступ к разделам OTA: `DFU` (куда пишется новый образ) и
/// `BOOTLOADER_STATE` (где лежит решение bootloader'а, что делать на
/// следующем сбросе). Границы обоих берутся из символов `memory.x`, то есть
/// из раскладки, посчитанной при генерации.
pub struct Ota {
    /// Тот же цельный `Flash`, что и в `cross/boot`: он сам знает реальные
    /// границы секторов чипа, в том числе неравномерные (F4/F7/H7), а
    /// банковые регионы у каждого семейства называются по-своему.
    flash: Mutex<NoopRawMutex, RefCell<Flash<'static, Blocking>>>,
    /// Буфер под одно слово записи во flash: `embassy-boot` пишет через него
    /// состояние, и выравнивание должно быть флешевым.
    aligned: AlignedBuffer<WRITE_SIZE>,
}

impl Ota {
    pub fn new(flash: Peri<'static, FLASH>) -> Self {
        Self {
            flash: Mutex::new(RefCell::new(Flash::new_blocking(flash))),
            aligned: AlignedBuffer([0; WRITE_SIZE]),
        }
    }

    /// Что bootloader сделает на следующем сбросе: [`State::Boot`] — запустит
    /// текущий образ, [`State::Swap`] — поменяет разделы местами,
    /// [`State::Revert`] — вернёт предыдущий (обновление не подтвердили).
    pub fn state(&mut self) -> Result<State, Error> {
        self.updater().get_state()
    }

    /// Пишет кусок нового образа в `DFU` по смещению от начала раздела.
    ///
    /// Стиранием секторов `embassy-boot` занимается сам, но длина куска
    /// должна быть кратна `WRITE_SIZE` флеша — обычно это и есть размер
    /// пакета, которым транспорт отдаёт данные.
    pub fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Error> {
        self.updater().write_firmware(offset, data)
    }

    /// Читает записанное обратно — проверить контрольную сумму принятого
    /// образа, не полагаясь на то, что запись прошла успешно.
    pub fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
        self.updater().read_dfu(offset, buf)
    }

    /// Просит bootloader поменять разделы местами на следующем сбросе.
    /// Сам сброс — за вызывающим: только он знает, когда устройство можно
    /// перезапустить.
    pub fn mark_updated(&mut self) -> Result<(), Error> {
        self.updater().mark_updated()
    }

    /// Подтверждает, что текущий образ работоспособен, и отменяет откат.
    /// Зовите из нового образа после того, как он это доказал — см. раздел
    /// про подтверждение в начале модуля.
    pub fn mark_booted(&mut self) -> Result<(), Error> {
        self.updater().mark_booted()
    }

    /// Апдейтер держит ссылки и на flash, и на буфер, поэтому хранить его
    /// рядом с ними одной структурой нельзя — она вышла бы самоссылающейся.
    /// Собирается он дёшево (чтение четырёх символов линкера), так что каждая
    /// операция делает себе свой; тип его при этом остаётся невыразимо
    /// длинным, отсюда и `impl Trait` вместо явной сигнатуры.
    fn updater(
        &mut self,
    ) -> BlockingFirmwareUpdater<'_, impl NorFlash + use<'_>, impl NorFlash + use<'_>> {
        let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&self.flash, &self.flash);
        BlockingFirmwareUpdater::new(config, &mut self.aligned.0)
    }
}
