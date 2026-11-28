#![doc = include_str!("../../../docs/modules/cpu-external.md")]

use core::any::TypeId;
use core::cell::RefCell;

use embassy_sync::blocking_mutex;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use static_cell::StaticCell;
{%- if ota == "true" and ota_store == "external" %}

/// Страница обмена, под которую посчитана раскладка; bootloader берёт её
/// отсюда — переменная среды `EXT_PAGE_SIZE` из `[env]`
/// `crates-cross/.cargo/config.toml`. С драйвером, чьё стирание её не делит,
/// страница обмена `embassy-boot` выросла бы, `ACTIVE` перестал бы быть ей
/// кратен, и bootloader паниковал бы в `assert_partitions` на каждом старте.
pub const EXT_PAGE_SIZE: usize = match usize::from_str_radix(env!("EXT_PAGE_SIZE"), 10) {
    Ok(size) => size,
    Err(_) => panic!("EXT_PAGE_SIZE в [env] crates-cross/.cargo/config.toml — не число"),
};

// Вдобавок к проверке в bootloader'е: у приложения свой тип драйвера
// (асинхронный — `app` оборачивает его `BlockingAsync`), и расходиться они могут.
const _: () = assert!(
    EXT_PAGE_SIZE.is_multiple_of(<ExternalFlash as NorFlash>::ERASE_SIZE),
    "стирание драйвера внешней флеши не делит страницу обмена (EXT_PAGE_SIZE)"
);
{%- endif %}

/// Что не работает, пока флеш не отвечает, — для строки в лог.
const LOST: &str = "{% if ota == "true" and ota_store == "external" %}OTA{% if config_store == "external" %} и настройки не работают{% else %} не работает{% endif %}{% else %}настройки не сохраняются{% endif %}";

/// Шина-заглушка: микросхема не подключена. Драйвер W25Q над ней настоящий,
/// а каждая команда отвечает [`NotConnected`]: `init` не проходит, и драйвер
/// отказывает в любой операции — {% if ota == "true" and ota_store == "external" %}образ не принимается{% endif %}{% if ota == "true" and ota_store == "external" and config_store == "external" %}, {% endif %}{% if config_store == "external" %}настройки не сохраняются{% endif %}, но ничего не
/// падает. Блокирующая — ею пользуются и приложение, и bootloader: тот с ней не
/// обменивает разделы, а загружает текущий образ.
///
/// Плата подставляет сюда {% if ext_flash_bus == "spi" %}`w25q::spi::SpiAdapter` над `SpiDevice` блока `{{ext_flash_block}}`{% else %}свою обёртку
/// над `embassy_stm32::{% if ext_flash_bus == "quadspi" %}qspi::Qspi{% else %}ospi::Ospi{% endif %}` блока `{{ext_flash_block}}`{% endif %}
/// (docs/modules/cpu-external.md).
#[derive(Debug, Default)]
pub struct StubBus;

/// Ответ [`StubBus`] на любую команду.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct NotConnected;

impl w25q::bus::FlashBusAsync for StubBus {
    type Error = NotConnected;

    /// Ширина настоящей шины: по ней драйвер выбирает команды{% if ext_flash_bus != "spi" %} (и ставит
    /// бит QE){% endif %}.
    const LANES: w25q::bus::Lanes = w25q::bus::Lanes::{% if ext_flash_bus == "spi" %}Single{% else %}Quad{% endif %};

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }
}

impl w25q::bus::FlashBusSync for StubBus {
    type Error = NotConnected;

    const LANES: w25q::bus::Lanes = <Self as w25q::bus::FlashBusAsync>::LANES;

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> Result<(), Self::Error> {
        Err(NotConnected)
    }
}

/// Шина микросхемы: по умолчанию — [`StubBus`]; плата подставляет свою.
pub type Bus = StubBus;

/// Драйвер внешней флеши: блокирующий W25Q над [`Bus`].
///
/// Задержки — на `SysTick` ядра: драйвер опрашивает занятость микросхемы, а
/// исполнителя у bootloader'а нет, и `embassy_time` там не тикает.
pub type ExternalFlash = w25q::device::W25qSync<Bus, cortex_m::delay::Delay>;

/// Отказ драйвера внешней флеши: причина не различается — разделы и так
/// сообщают о ней одной строкой в лог.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct ExtFlashError;

impl From<w25q::error::Error<NotConnected>> for ExtFlashError {
    fn from(_: w25q::error::Error<NotConnected>) -> Self {
        Self
    }
}

impl NorFlashError for ExtFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::Other
    }
}

/// Состояние внешней флеши в статике: опознанный драйвер или отказ.
///
/// Разделы строятся над `NorFlash`, а не над драйвером: после неудачного
/// `init` (микросхемы нет, она меньше разделов) драйвер обязан отказывать в
/// любой операции, а не паниковать, и делать это умеет только обёртка.
pub enum ExtFlashState {
    /// Микросхема опознана (или ещё не опознана — `init` не проходил).
    Ready(ExternalFlash),
    /// Флеш не годится: каждая операция отказывает.
    Broken,
}

// Вручную: у `Delay` из `cortex-m` нет `Debug`.
impl core::fmt::Debug for ExtFlashState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Ready(_) => f.debug_tuple("Ready").finish_non_exhaustive(),
            Self::Broken => f.write_str("Broken"),
        }
    }
}

impl ErrorType for ExtFlashState {
    type Error = ExtFlashError;
}

impl ReadNorFlash for ExtFlashState {
    const READ_SIZE: usize = <ExternalFlash as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        match self {
            Self::Ready(driver) => driver.read(offset, bytes).map_err(ExtFlashError::from),
            Self::Broken => Err(ExtFlashError),
        }
    }

    fn capacity(&self) -> usize {
        match self {
            Self::Ready(driver) => driver.capacity() as usize,
            Self::Broken => 0,
        }
    }
}

impl NorFlash for ExtFlashState {
    const WRITE_SIZE: usize = <ExternalFlash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <ExternalFlash as NorFlash>::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        match self {
            Self::Ready(driver) => driver.erase(from, to).map_err(ExtFlashError::from),
            Self::Broken => Err(ExtFlashError),
        }
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        match self {
            Self::Ready(driver) => driver.write(offset, bytes).map_err(ExtFlashError::from),
            Self::Broken => Err(ExtFlashError),
        }
    }
}

/// Внешняя флеш под блокирующим замком — то, над чем строятся её разделы.
///
/// `ThreadModeRawMutex` — то же допущение «один исполнитель», что у
/// `crate::flash::Lock`; менять вместе.
pub type ExternalMutex = blocking_mutex::Mutex<ThreadModeRawMutex, RefCell<ExtFlashState>>;

/// Ручка на внешнюю флеш — то, что `Cpu::split` отдаёт плате.
///
/// `Copy` намеренно: флеш одна, а нужна она и обновлению, и настройкам; плата
/// отдаёт ручку в оба, а не двигает объект между ними.
#[derive(Clone, Copy)]
pub struct ExtFlash {
    /// Замок и состояние драйвера.
    pub lock: &'static ExternalMutex,
}

// Вручную: у `Mutex` нет `Debug`, да и печатать замок незачем.
impl core::fmt::Debug for ExtFlash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExtFlash").finish_non_exhaustive()
    }
}

/// `&'static` он не для красоты: флешь отдаётся в задачи, а аргументы задач
/// embassy обязаны быть `'static`. Обычное поле `Board` этого не даёт —
/// `Board` живёт в `main`, а не вечно.
static EXTERNAL: StaticCell<ExternalMutex> = StaticCell::new();

/// Подставлена ли шина вместо заглушки — для target-тестов, которым без
/// микросхемы проверять нечего.
#[must_use]
pub fn connected() -> bool {
    TypeId::of::<Bus>() != TypeId::of::<StubBus>()
}

/// Собирает драйвер и кладёт его в статику — без единой команды микросхеме.
///
/// Опознаёт её [`init`] — его зовёт `app`, когда знает, сколько на флеши лежит
/// (границы разделов — знание приложения). Так `cpu::init` не делает ввода-вывода
/// и годится bootloader'у, у которого исполнителя нет.
///
/// Задержки драйвера — от частоты ядра `sys_hz`.
///
/// # Panics
///
/// Если периферию ядра уже забрали: без `SysTick` драйвер не выждет таймаутов
/// микросхемы.
pub(crate) fn new(_resources: crate::wiring::ExtFlashResources, sys_hz: u32) -> ExtFlash {
    let core = cortex_m::Peripherals::take().expect("cpu: периферия ядра уже взята");
    // Шина из шаблона — заглушка: `Bus` — псевдоним, значения у него нет.
    // Плата подставляет свою шину — вместе с этой строкой (рецепт —
    // docs/modules/cpu-external.md).
    let driver = ExternalFlash::new(StubBus, cortex_m::delay::Delay::new(core.SYST, sys_hz));
    let lock = EXTERNAL.init(blocking_mutex::Mutex::new(RefCell::new(
        ExtFlashState::Ready(driver),
    )));
    ExtFlash { lock }
}

/// Опознаёт микросхему (`init`: JEDEC ID, объём{% if ext_flash_bus != "spi" %}, бит QE{% endif %}) и сверяет с
/// разделами.
///
/// Не прошёл `init` или микросхема меньше разделов — одна строка в лог, и
/// драйвер отказывает в любой операции ошибкой, а не паникой. Плата без флеши
/// (или с заглушкой шины) работает, только без того, что на флеши лежит.
///
/// # Panics
///
/// Если объём микросхемы не помещается в `u32`: предел разделов в
/// `ports::Partitions` — `u32`, а микросхем такого размера нет.
pub fn init(bus: ExtFlash, required: u32) {
    bus.lock.lock(|cell| {
        let state = &mut *cell.borrow_mut();
        let ExtFlashState::Ready(driver) = state else {
            return;
        };
        let outcome = match driver.init() {
            Ok(_) => Ok(u32::try_from(driver.capacity()).expect("объём внешней флеши — u32")),
            Err(err) => Err(err),
        };
        match outcome {
            Ok(capacity) if capacity >= required => {
                defmt::info!("cpu: внешняя флеш W25Q, {=u32} KiB", capacity / 1024);
            }
            Ok(capacity) => {
                defmt::error!(
                    "cpu: внешняя флеш {=u32} KiB, а разделам нужно {=u32} KiB — {=str}",
                    capacity / 1024,
                    required / 1024,
                    LOST
                );
{%- if ota == "true" and ota_store == "external" %}
                // Не оставлять опознанный драйвер: с ним образ лёг бы в начало
                // `DFU`, которого на микросхеме не хватает, а bootloader на обмене
                // упёрся бы в её конец. Неопознанный отказывает во всём.
{%- else %}
                // Не оставлять опознанный драйвер: с ним раздел настроек упёрся бы
                // в конец микросхемы. Неопознанный отказывает во всём.
{%- endif %}
                *state = ExtFlashState::Broken;
            }
            Err(_) if !connected() => {
                defmt::warn!(
                    "cpu: внешняя флеш не подключена — {=str} (cpu/src/external.rs)",
                    LOST
                );
            }
            Err(err) => {
                defmt::error!("cpu: внешняя флеш не отвечает: {} — {=str}", err, LOST);
            }
        }
    });
}
