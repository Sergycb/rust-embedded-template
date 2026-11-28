//! Разделы обновления и настроек — их строит приложение над `Copy`-ручками
//! чипа (`Board::flash`, `Board::ext_flash_bus`).
//!
//! Почему здесь, а не в `cpu`: границы разделов — символы `memory.x` этого
//! бинарника (`crate::layout`), а раскладка памяти принадлежит приложению.
//! Ручки чипа (`cpu::flash::Flash`, `cpu::external::ExtFlash`) — `Copy`, так
//! что обновление и настройки получают один и тот же контроллер, а не делят
//! объект.
{%- assign need_backing = "false" %}
{%- if ota == "true" or config_store == "flash" or config_store == "external" %}
{%- assign need_backing = "true" %}
{%- endif %}
{%- assign need_whole = "false" %}
{%- if ota == "true" and ota_store != "external" %}
{%- assign need_whole = "true" %}
{%- endif %}
{%- if config_store == "flash" %}
{%- assign need_whole = "true" %}
{%- endif %}
{%- assign need_registry = "false" %}
{%- if config_store == "flash" or config_store == "external" %}
{%- assign need_registry = "true" %}
{%- endif %}
{%- if need_registry == "true" %}

use core::cell::Cell;

use cortex_m::interrupt;
{%- endif %}
{%- if need_backing == "true" %}

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_embedded_hal::flash::partition::Partition;
use embassy_sync::mutex::Mutex as AsyncMutex;
use static_cell::StaticCell;
{%- endif %}
{%- if erase_page == "true" %}

use flash_page::ErasePage;
{%- endif %}
{%- if external_flash == "true" %}

use embedded_storage::nor_flash::ReadNorFlash as _;
{%- endif %}
{%- if state_paged == "true" %}

use cpu::flash::STATE_ERASE_SIZE;
{%- endif %}
{%- if swap_paged == "true" %}

use cpu::flash::SWAP_PAGE_SIZE;
{%- endif %}
{%- if config_store == "flash" %}

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7) со страницей всего чипа не делится на две, поэтому
/// страница своя{% if config_paged == "true" %} (`ErasePage`){% endif %}; в хвосте flash она совпадает со
/// страницей чипа{% if config_paged != "true" %} — как у этого чипа: раздел строится прямо над
/// [`Whole`], а равенство держит `const`-assert ниже{% endif %}.
/// Число посчитано при генерации тем же расчётом, что адреса в `memory.x`; тем же
/// литералом там стоит `ASSERT` — границы `CONFIG` кратны ей, иначе
/// `Partition::new` паниковал бы на старте, а так падает линковка.
const CONFIG_ERASE_SIZE: usize = {{config_erase_size}};
{%- if config_paged != "true" %}

const _: () = assert!(
    CONFIG_ERASE_SIZE == cpu::flash::MAX_ERASE_SIZE,
    "CONFIG_ERASE_SIZE из генерации не равен странице цельного `Flash` \
     (cpu::flash::MAX_ERASE_SIZE): `CONFIG` нужна своя страница"
);
{%- endif %}
{%- endif %}
{%- if need_whole == "true" %}

/// Весь флеш чипа как один раздел: смещения те же, что у самого флеша, а
/// операции идут под замком контроллера. Из него строятся разделы со своей
/// страницей (`ErasePage`).
type Whole = BlockingPartition<'static, crate::Mutex, cpu::flash::ChipFlash>;
{%- endif %}
{%- if ota_store == "external" or config_store == "external" %}

/// Раздел на всю внешнюю флешь — носитель её разделов.
type ExtBacking =
    BlockingAsync<BlockingPartition<'static, crate::Mutex, cpu::external::ExtFlashState>>;
{%- endif %}
{%- if ota == "true" %}
{%- if ota_store == "external" %}

/// Носитель разделов на внешней флеши — раздел на всю микросхему.
type Backing = ExtBacking;

/// Носитель `DFU` — тот же раздел.
type DfuFlash = Backing;

/// Носитель `BOOTLOADER_STATE` — тот же раздел.
type StateFlash = Backing;
{%- elsif dfu_bank == "true" %}

/// Носитель `DFU` — банк 2 за разрывом: смещения `memory.x` отсчитаны от его
/// начала.
type DfuFlash = BlockingAsync<BlockingPartition<'static, crate::Mutex, cpu::flash::Bank2Flash>>;
{%- if state_paged == "true" %}

/// Носитель `BOOTLOADER_STATE` — банк 1 со страницей своего сектора.
type StateFlash = BlockingAsync<ErasePage<Whole, STATE_ERASE_SIZE>>;
{%- else %}

/// Носитель `BOOTLOADER_STATE` — банк 1: страница сектора `STATE` совпадает с
/// его страницей.
type StateFlash = BlockingAsync<Whole>;
{%- endif %}
{%- elsif swap_paged == "true" %}

/// Носитель `DFU` — весь флеш со страницей обмена, крупнее физической.
type DfuFlash = BlockingAsync<ErasePage<Whole, SWAP_PAGE_SIZE>>;
{%- if state_paged == "true" %}

/// Носитель `BOOTLOADER_STATE` — со страницей своего сектора.
type StateFlash = BlockingAsync<ErasePage<Whole, STATE_ERASE_SIZE>>;
{%- else %}

/// Носитель `BOOTLOADER_STATE` — страница сектора `STATE` совпадает с его
/// страницей.
type StateFlash = BlockingAsync<Whole>;
{%- endif %}
{%- else %}

/// Носитель `DFU` — весь флеш чипа.
type DfuFlash = BlockingAsync<Whole>;
{%- if state_paged == "true" %}

/// Носитель `BOOTLOADER_STATE` — со страницей своего сектора.
type StateFlash = BlockingAsync<ErasePage<Whole, STATE_ERASE_SIZE>>;
{%- else %}

/// Носитель `BOOTLOADER_STATE` — тот же весь флеш: страницы совпадают.
type StateFlash = BlockingAsync<Whole>;
{%- endif %}
{%- endif %}

/// Разделы обновления — рычаг под адаптер обновления (`domain::ota::Updater`):
/// `DFU` и `BOOTLOADER_STATE`, нарезанные по символам `memory.x`
/// (`crate::layout`), со своими страницами, размер `ACTIVE` и отпечаток
/// раскладки.
pub type Ota = ports::Partitions<
    Partition<'static, crate::Mutex, DfuFlash>,
    Partition<'static, crate::Mutex, StateFlash>,
>;
{%- endif %}
{%- if config == "true" %}
{%- if config_store == "flash" %}

/// Носитель раздела `CONFIG`{% if config_paged == "true" %} со страницей своего сектора{% else %} — страница совпадает с его
/// страницей{% endif %}.
type ConfigBacking = BlockingAsync<{% if config_paged == "true" %}ErasePage<Whole, CONFIG_ERASE_SIZE>{% else %}Whole{% endif %}>;
{%- elsif config_store == "eeprom" %}

/// Внутренняя EEPROM как `NorFlash` — её отдаёт чип (`cpu::eeprom`).
pub type ConfigFlash = cpu::eeprom::Eeprom<crate::Mutex>;
{%- else %}

/// Носитель раздела настроек на внешней флеши — раздел на всю микросхему.
type ConfigBacking = ExtBacking;
{%- endif %}
{%- if config_store != "eeprom" %}

/// Раздел `CONFIG` — носитель хранилища настроек (`domain::settings`).
/// Именно раздел, а не весь флеш: запись за его границы отказывает, а не
/// стирает {% if ota == "true" %}`ACTIVE` или bootloader{% else %}прошивку{% endif %}.
pub type ConfigFlash = Partition<'static, crate::Mutex, ConfigBacking>;
{%- endif %}

/// Хранилище настроек над разделом (или EEPROM) этого проекта.
pub type Settings = domain::settings::Settings<ConfigFlash>;
{%- endif %}
{%- if ota == "true" %}

/// Свои асинхронные мьютексы носителям `DFU` и `BOOTLOADER_STATE`:
/// `domain::ota::Updater` строит каждый раздел из `&Mutex<_, T>` своего типа.
/// Внутри обоих — ссылка на один контроллер чипа, а не второй флеш.
static DFU_FLASH: StaticCell<AsyncMutex<crate::Mutex, DfuFlash>> = StaticCell::new();
static STATE_FLASH: StaticCell<AsyncMutex<crate::Mutex, StateFlash>> = StaticCell::new();
{%- endif %}
{%- if need_registry == "true" %}

/// Свой асинхронный мьютекс разделу `CONFIG`: `Partition` строится из
/// `&Mutex<_, T>`, а хранилище создаётся у каждого потребителя своё. Один
/// раздел на запуск — один и мьютекс: `StaticCell::init` второй раз паникует.
static CONFIG_REF: interrupt::Mutex<
    Cell<Option<&'static AsyncMutex<crate::Mutex, ConfigBacking>>>,
> = interrupt::Mutex::new(Cell::new(None));
static CONFIG_MUTEX: StaticCell<AsyncMutex<crate::Mutex, ConfigBacking>> = StaticCell::new();
{%- endif %}
{%- if need_whole == "true" %}

/// Весь флеш чипа как раздел — носитель разделов со своей страницей.
fn whole(flash: cpu::flash::Flash) -> Whole {
    BlockingPartition::new(flash.lock, 0, cpu::flash::SIZE)
}
{%- endif %}
{%- if ota_store == "external" or config_store == "external" %}

/// Раздел на всю опознанную микросхему: её объём и есть предел.
fn ext_backing(ext: cpu::external::ExtFlash) -> ExtBacking {
    let capacity: u32 = ext
        .lock
        .lock(|state| u32::try_from(state.borrow().capacity()).expect("объём внешней флеши — u32"));
    BlockingAsync::new(BlockingPartition::new(ext.lock, 0, capacity))
}
{%- endif %}
{%- if ota == "true" %}
{%- if ota_store == "external" %}

/// Носитель `DFU` — та же внешняя флешь.
fn dfu_flash(ext: cpu::external::ExtFlash) -> DfuFlash {
    ext_backing(ext)
}
{%- elsif dfu_bank == "true" %}

/// Носитель `DFU` — банк 2.
fn dfu_flash(flash: cpu::flash::Flash) -> DfuFlash {
    BlockingAsync::new(BlockingPartition::new(
        flash.bank2,
        0,
        cpu::flash::BANK2_SIZE,
    ))
}
{%- else %}

/// Носитель `DFU` — весь флеш или его страница обмена.
fn dfu_flash(flash: cpu::flash::Flash) -> DfuFlash {
{%- if swap_paged == "true" %}
    BlockingAsync::new(ErasePage::new(whole(flash)))
{%- else %}
    BlockingAsync::new(whole(flash))
{%- endif %}
}
{%- endif %}
{%- if ota_store == "external" %}

/// Носитель `BOOTLOADER_STATE` — та же внешняя флешь.
fn state_flash(ext: cpu::external::ExtFlash) -> StateFlash {
    ext_backing(ext)
}
{%- else %}

/// Носитель `BOOTLOADER_STATE` — весь флеш или его страница сектора.
fn state_flash(flash: cpu::flash::Flash) -> StateFlash {
{%- if state_paged == "true" %}
    BlockingAsync::new(ErasePage::new(whole(flash)))
{%- else %}
    BlockingAsync::new(whole(flash))
{%- endif %}
}
{%- endif %}

/// Собирает разделы обновления над ручками чипа. Зовётся один раз за запуск.
///
/// # Panics
///
/// Если разделы уже собирали: `spawn_all` зовётся один раз за запуск.
#[must_use]
pub fn ota(
    // Ручка чипа: разделы строятся над её контроллером.
{%- if ota_store == "external" %}
    _flash: cpu::flash::Flash,
    ext: cpu::external::ExtFlash,
{%- elsif external_flash == "true" %}
    flash: cpu::flash::Flash,
    // Внешняя флеш в этом проекте есть, но разделы OTA — во внутренней:
    // ручка нужна только вызывающему, разделы её не трогают.
    _ext: cpu::external::ExtFlash,
{%- else %}
    flash: cpu::flash::Flash,
{%- endif %}
) -> Ota {
{%- if ota_store == "external" %}
    let dfu = DFU_FLASH.init(AsyncMutex::new(dfu_flash(ext)));
    let state = STATE_FLASH.init(AsyncMutex::new(state_flash(ext)));
{%- else %}
    let dfu = DFU_FLASH.init(AsyncMutex::new(dfu_flash(flash)));
    let state = STATE_FLASH.init(AsyncMutex::new(state_flash(flash)));
{%- endif %}
    let (dfu_start, dfu_len) = crate::layout::dfu();
    let (state_start, state_len) = crate::layout::state();
    Ota {
        dfu: Partition::new(dfu, dfu_start, dfu_len),
        state: Partition::new(state, state_start, state_len),
        capacity: crate::layout::active_len(),
        layout: crate::layout::fingerprint(),
    }
}
{%- endif %}
{%- if need_registry == "true" %}

/// Мьютекс раздела `CONFIG` — один на запуск; `init` зовётся, только если его
/// ещё нет (второго `StaticCell::init` быть не должно).
fn config_mutex(
    init: impl FnOnce() -> ConfigBacking,
) -> &'static AsyncMutex<crate::Mutex, ConfigBacking> {
    interrupt::free(|cs| {
        if let Some(existing) = CONFIG_REF.borrow(cs).get() {
            return existing;
        }
        let created: &'static AsyncMutex<crate::Mutex, ConfigBacking> =
            CONFIG_MUTEX.init(AsyncMutex::new(init()));
        CONFIG_REF.borrow(cs).set(Some(created));
        created
    })
}
{%- endif %}
{%- if config == "true" %}
{%- if config_store == "flash" %}

/// Носитель раздела `CONFIG` — от [`Whole`] или его страницы.
fn config_backing(flash: cpu::flash::Flash) -> ConfigBacking {
{%- if config_paged == "true" %}
    BlockingAsync::new(ErasePage::new(whole(flash)))
{%- else %}
    BlockingAsync::new(whole(flash))
{%- endif %}
}
{%- endif %}

/// Собирает хранилище настроек над ручками чипа.
///
/// Не один раз за запуск, а сколько позовут: раздел общий (`CONFIG_MUTEX`),
/// а `Settings` — свой у каждого потребителя (у узла `CANopen` и у ключа хоста
/// SSH).
#[must_use]
pub fn settings(
    // Ручка чипа: носитель настроек строится над ней.
{%- if config_store == "external" %}
    _flash: cpu::flash::Flash,
    ext: cpu::external::ExtFlash,
{%- else %}
    flash: cpu::flash::Flash,
{%- if external_flash == "true" %}
    _ext: cpu::external::ExtFlash,
{%- endif %}
{%- endif %}
) -> Settings {
{%- if config_store == "flash" %}
    let backing = config_mutex(|| config_backing(flash));
    let (start, len) = crate::layout::config();
    Settings::whole(Partition::new(backing, start, len))
{%- elsif config_store == "eeprom" %}
    Settings::whole(cpu::eeprom::new(flash.lock))
{%- else %}
    let backing = config_mutex(|| ext_backing(ext));
    let (start, len) = crate::layout::config();
    Settings::whole(Partition::new(backing, start, len))
{%- endif %}
}
{%- endif %}
