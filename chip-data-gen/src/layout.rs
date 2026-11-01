//! Раскладка памяти чипа для `memory.x`: `FLASH`/`RAM`, разделы OTA и
//! настроек, `PANIC`, `DMA_NC` — и проверки инвариантов `embassy-boot` над ними.

use std::collections::BTreeSet;

use crate::metadata::{RawRegion, RegionKind};

/// Самое мелкое стирание, которое раскладка ждёт от внешней флеши: сектор
/// 4 KiB есть у последовательной NOR всех крупных производителей (W25Q, MX25,
/// GD25, IS25, AT25). `ACTIVE` кратен ему: обмен идёт страницами
/// `max(ACTIVE::ERASE_SIZE, DFU::ERASE_SIZE)`, и драйвер с более крупным
/// стиранием потребовал бы `ACTIVE` кратного уже его странице.
const EXT_ERASE_MIN: u64 = 4 * 1024;

/// Шаг разделов на внешней флеши: блок 64 KiB — самое крупное стирание,
/// которое бывает у той же NOR. Внешняя память дешёвая, и запас здесь ничего
/// не стоит, а разделы остаются годными для драйвера с любым стиранием до
/// блока.
const EXT_ALIGN: u64 = 64 * 1024;

/// Самое широкое слово записи, на которое считается журнал `STATE`: то же,
/// что `ports::MAX_WRITE_SIZE`, — буфер шире адаптер всё равно не
/// держит.
const EXT_WRITE_MAX: u64 = 32;

const FLASH_BASE: u64 = 0x0800_0000;

const RAM_BASE: u64 = 0x2000_0000;

/// Регионы, которые `stm32-metapac` перечисляет как окна ВНЕШНИХ шин: реальной
/// памяти за ними нет, пока к плате не припаяна микросхема и не настроен
/// контроллер. Метка видна и по размеру — у всех у них он одинаковый,
/// 262144000 (не память, а максимум адресного окна). Такие регионы попадают в
/// `memory.x` закомментированными: перечислить их полезно, а объявить рабочей
/// памятью — значит подсунуть линкеру то, чего на плате может не быть вовсе.
const EXTERNAL_BUS_PREFIXES: &[&str] =
    &["FMC_", "SDRAM_", "OCTOSPI_", "QUADSPI_", "HSPI_", "XSPI_"];

/// Суффикс второго (I-Code) окна того же физического блока RAM: `CCMRAM_ICODE`
/// — та же память, что `CCMRAM_DCODE`, `SRAM2_ICODE` — та же, что `SRAM2`.
/// Тоже закомментированы: разместить данные в обоих окнах разом — молчаливое
/// наложение, которое линкер не поймает (адреса-то разные).
const ALIAS_SUFFIX: &str = "_ICODE";

/// Ориентир на размер бинарника bootloader (embassy-boot + defmt +
/// cortex-m-rt) — на практике укладывается в 20-30 KiB, отсюда небольшой
/// запас. Bootloader сам по себе не стирается во время работы, поэтому ему
/// не нужно быть кратным `PAGE_SIZE` — только `BOOTLOADER_STATE` после него.
const BOOTLOADER_TARGET: u64 = 32 * 1024;

/// Сколько от зарезервированного блока обязано остаться под сам бинарник
/// после того, как из него вычли `BOOTLOADER_STATE`. Меньше — резерв растёт
/// ещё на страницу. 26 KiB: подписанный release-`boot` весит ≈23 KiB (подпись
/// обязательна при OTA), остальное — запас на рост.
const BOOTLOADER_MIN: u64 = 26 * 1024;

/// На сколько страниц резерв может вырасти СВЕРХ исходного (`BOOTLOADER_TARGET`,
/// округлённого вверх до страницы). Именно сверх, а не всего: у чипа с мелкими
/// секторами исходный резерв сам по себе занимает десятки страниц (32 KiB при
/// секторе 256 байт у L0/L1 — 128 штук), и абсолютный предел отрезал бы от OTA
/// всю мелкосекторную половину линейки. У крупных страниц предел — эти 16
/// страниц, у мелких его поднимает [`MAX_EXTRA_RESERVED_BYTES`].
const MAX_EXTRA_RESERVED_PAGES: u64 = 16;

/// Рост резерва не меньше этого объёма независимо от размера страницы: 16
/// страниц по 128 байт — всего 2 KiB, а `STATE` на такой странице может занять
/// и больше.
const MAX_EXTRA_RESERVED_BYTES: u64 = 8 * 1024;

/// Бюджет `BOOTLOADER_STATE` под журнал обмена: пока он не больше, страницу
/// обмена не расширяют. Журнал — `(2 + 4*N) * WRITE_SIZE` байт при N страницах
/// обмена в `ACTIVE` (`assert_partitions`), то есть растёт с flash и убывает с
/// размером страницы: L0 (страница 128 байт, ~640 страниц) без расширения
/// отдавал под `STATE` 10 KiB из 32 KiB, и подписанный bootloader не влезал.
const STATE_BUDGET: u64 = 2 * 1024;

/// Самая крупная страница обмена, до которой её расширяют ради `STATE_BUDGET`.
/// Больше — крупнее разменная страница `DFU` и грубее потери на хвосте flash.
const SWAP_PAGE_MAX: u64 = 8 * 1024;

/// От конца RAM отрезается регион `PANIC` под дамп `panic-persist` —
/// паникёр release-профиля пишет туда по голым адресам
/// `_panic_dump_start.._panic_dump_end`, ничего не зная о секциях (секция у
/// верхней границы RAM сломала бы `flip-link`, см. комментарий в
/// `chip-select.rhai`).
///
/// Размер — доля RAM, а не константа: у чипа с 2 KiB (STM32L011 и родня)
/// фиксированные полкилобайта были бы четвертью всей памяти. Регион нужен
/// каждому чипу: без символов дампа `panic-persist` не линкуется, а он стоит у
/// `app` и `boot` безусловно (в release).
const PANIC_FRACTION: u64 = 16;

/// Нижняя граница: восемь байт съедает заголовок дампа, дальше текст — на
/// «panicked at src/main.rs:42:5» хватает, длинное сообщение обрежется.
const PANIC_MIN: u64 = 128;

/// Больше полукилобайта отдавать незачем: сообщение всё равно обрезается по
/// размеру региона.
const PANIC_MAX: u64 = 512;

/// Размеры региона `DMA_NC` в хвосте RAM по убыванию. Регион MPU Armv7-M —
/// степень двойки не меньше 32 байт, адрес кратен размеру; берётся наибольший,
/// при котором конец RAM даёт такой адрес. 32 KiB хватает на очереди Ethernet и
/// буферы SDMMC, меньшие — запасной вариант для «кривого» конца.
const DMA_NC_SIZES: [u64; 4] = [32 * 1024, 16 * 1024, 8 * 1024, 4 * 1024];

/// Адрес `SRAM4` домена D3 у H7 — память, до которой дотягивается только BDMA.
const H7_SRAM4: u64 = 0x3800_0000;

/// Начало SRAM домена D2 у H7 (`SRAM1..3`) — RAM ядра CM4 двухъядерных чипов.
const H7_D2_SRAM: u64 = 0x3000_0000;

/// Где у STM32WB начинается flash CPU2, по объёму flash: `(объём, адрес)`.
///
/// Flash от `SFSA × 4K` до конца закрыт для CPU1 (RM0434 §3.6.4: доступ —
/// `RDERR`/`WRPERR`), и там всегда сидит FUS, а со стеком — и стек (AN5185
/// §1.4, Figure 1). Сам `SFSA` прошивка не выбирает: его ставит FUS при
/// установке стека. Поэтому граница — адрес установки
/// `stm32wb5x_BLE_Stack_full_fw.bin` из Release Notes STM32WB Copro Wireless
/// Binaries v1.24.0 (Wireless Coprocessor Binary Table): xG (1M), xY (640K),
/// xE (512K), xC (256K). Те же столбцы — у WB30/WB35 (тот же кристалл,
/// RM0434) и WB50 (столбец xG). Стек крупнее (`BLE_Mac_802_15_4`, Thread,
/// Zigbee) ставится ниже, и тогда границу опускают руками — docs/flash.md.
///
/// WB10/WB15 (320K, сектор 2K) в этих Release Notes нет (у них свои
/// бинарники копроцессора), и граница не сверена с первоисточником: взята с
/// тем же запасом от конца flash, что у xG (192K), — на 152K ниже FUS
/// (`0x0804_6000`, AN5185 §2.2), тогда как у `WB5x` между `BLE_Stack_full` и
/// FUS 120K.
const WB_CPU2_FLASH: [(u64, u64); 5] = [
    (256 * 1024, 0x0801_C000),
    (320 * 1024, 0x0802_0000),
    (512 * 1024, 0x0805_C000),
    (640 * 1024, 0x0807_C000),
    (1024 * 1024, 0x080D_0000),
];

/// Чип, у которого RAM меньше двух минимальных резервов, автораскладки не
/// получает вовсе (и генератор на нём падает). Среди STM32 таких нет
/// — самый скромный вариант это 2 KiB, — но инвариант «есть раскладка →
/// есть PANIC» должен держаться без оговорок.
const MIN_RAM_FOR_LAYOUT: u64 = 2 * PANIC_MIN;

/// Готовые для `memory.x` адреса/размеры одного чипа. Всё в байтах —
/// `format_memory_layouts` переводит в hex/`K`-нотацию для Rhai.
pub(crate) struct MemoryLayout {
    pub(crate) flash_origin: u64,
    pub(crate) flash_length: u64,
    pub(crate) ram_origin: u64,
    pub(crate) ram_length: u64,
    /// Хвост RAM под дамп `panic-persist` — единственный резерв в RAM. Всегда
    /// есть, если есть раскладка: без символов `_panic_dump_*` release-образ
    /// `app` не слинкуется (см. `PANIC_MIN`).
    pub(crate) panic: (u64, u64),
    /// Готовые строки `MEMORY { }` для всех прочих регионов чипа (ITCM, DTCM,
    /// CCMRAM, BKPSRAM, EEPROM, OTP, окна внешних шин...) — см.
    /// `extra_region_lines`. Одинаковы для `app` и `boot`.
    pub(crate) extra_regions: Vec<String>,
    /// Const generic `BootLoader::prepare::<_, _, _, N>` — размер буфера
    /// подкачки. Требования к нему (`prepare_boot` в `embassy-boot`):
    /// делит `PAGE_SIZE`, кратен `NorFlash::WRITE_SIZE` и не меньше него.
    /// `NorFlash::WRITE_SIZE` чипа удовлетворяет всем трём, его и берём.
    /// Имя плейсхолдера в шаблоне (`write_size`) осталось прежним ради
    /// совместимости с проектами, задающими его через `--define`.
    pub(crate) write_size: u64,
    /// Физическая страница чипа: максимальный сектор стирания по цепочке
    /// flash. Страница обмена `embassy-boot` (`PAGE_SIZE`) — она же, пока
    /// прошивка не расширила её (`OtaPartitions::swap_page_size`, плейсхолдер
    /// `{{page_size}}`): по ней bootloader переносит разделы, а host-target
    /// тест ищет, куда уехал прежний образ — `ACTIVE[i]` оказывается в
    /// `DFU[i + 1]`, то есть со сдвигом ровно на страницу обмена.
    pub(crate) page_size: u64,
    /// Стёртая страница читается нулями, а не `0xFF`.
    ///
    /// Берётся из `erase_value` в метаданных, а не выводится по имени чипа, и
    /// это не педантизм: раньше признак считался как «фича начинается на
    /// `l0`/`l1`», и под эвристику не попадали STM32WB10CC/WB15CC — у них
    /// `erase_value: 0` при совсем другом префиксе. Такому чипу доставался
    /// `embassy-boot` без фичи `flash-erase-zero` (обмен разделов читает
    /// признак «стёрто» наоборот) и раздел настроек, который не работает
    /// вовсе. Обе поломки молчаливые.
    pub(crate) erase_zero: bool,
    /// Начало второго банка, если между банками разрыв (H743 с 1 MiB и т.п.).
    /// Цельный `Flash` embassy туда не дотягивается — его `FLASH_SIZE` равен
    /// сумме банков, а смещение банка 2 больше неё, — и `DFU` берётся через
    /// регион банка (`Bank2Region`), со смещениями от этого адреса.
    pub(crate) dfu_bank_origin: Option<u64>,
    /// Non-cacheable память под буферы DMA — см. [`dma_nc_region`]; из блока-донора
    /// в `extra_regions` (или из `ram_length`) она уже вычтена.
    pub(crate) dma_nc: Option<DmaNc>,
    /// `SRAM4` целиком под буферы BDMA (`origin`, `length`) — см. [`bdma_nc_region`].
    pub(crate) bdma_nc: Option<(u64, u64)>,
    /// `Ok` — OTA-схема помещается в flash; `Err` — не помещается, и текст
    /// объясняет почему (печатается при генерации и попадает комментарием в
    /// `memory.x`). Во втором случае проект собирается одним образом на весь
    /// flash, без `crates-cross/boot`.
    pub(crate) ota: Result<OtaPartitions, String>,
    /// Раздел под настройки, если он на этом чипе вообще возможен. `None` —
    /// две страницы стирания не остаются даже без всего остального.
    pub(crate) config: Option<ConfigPartition>,
    /// OTA с `DFU` и `STATE` на внешней флеши: внутри только bootloader и
    /// `ACTIVE`. `Err` — даже так не помещается; текст объясняет почему.
    /// Настройки при внешней флеши — только на ней (`ExternalOta::config_*`):
    /// хук выбирает внешнее хранилище, как только есть `ext_flash=yes`.
    pub(crate) external: Result<ExternalOta, String>,
}

/// OTA с разделами на внешней флеши: во внутренней — bootloader и `ACTIVE`
/// (он и обязан быть внутри: из него исполняется код), на внешней — `STATE`,
/// `DFU` и, если нужен, раздел настроек.
///
/// Геометрию внешней микросхемы генератор не знает — её несёт драйвер,
/// который подставит проект. Поэтому всё внешнее выровнено с запасом
/// (`ext_align`), а сколько раскладка требует от драйвера, сказано явно
/// (`page_size`): его `ERASE_SIZE` обязан этот размер делить. Проверяет это
/// `const`-assert рядом с драйвером, а не генератор.
#[derive(Debug)]
pub(crate) struct ExternalOta {
    /// `FLASH` в `crates-cross/boot/memory.x`: от базы flash до `ACTIVE`.
    pub(crate) bootloader_length: u64,
    pub(crate) active_origin: u64,
    pub(crate) active_length: u64,
    /// `PAGE_SIZE` обмена в предположении, что стирание драйвера его делит:
    /// крупнейший сектор чипа, но не меньше `EXT_ERASE_MIN`.
    pub(crate) page_size: u64,
    /// Шаг, по которому выровнены смещения и размеры на внешней флеши.
    pub(crate) ext_align: u64,
    /// Смещения — от начала внешней флеши, а не адреса.
    pub(crate) state_offset: u64,
    pub(crate) state_length: u64,
    pub(crate) dfu_offset: u64,
    pub(crate) dfu_length: u64,
    /// Раздел настроек на внешней флеши — сразу за `DFU`; две страницы по
    /// `ext_align`. Используется, только если настройки выбраны.
    pub(crate) config_offset: u64,
    pub(crate) config_length: u64,
}

/// Партиции OTA-схемы (`crates-cross/boot`). Есть только у чипов, где схема
/// физически помещается — см. `compute_ota_partitions`.
#[derive(Debug)]
pub(crate) struct OtaPartitions {
    /// `FLASH` в `crates-cross/boot/memory.x`: место под сам бинарник bootloader'а,
    /// от базы flash до `BOOTLOADER_STATE`. Отдельный регион (а не весь чип)
    /// нужен, чтобы переполнение ловил линкер, а не молчаливое наложение на
    /// `ACTIVE`.
    pub(crate) bootloader_length: u64,
    pub(crate) bootloader_state_origin: u64,
    pub(crate) bootloader_state_length: u64,
    /// Сектор, в котором лежит `BOOTLOADER_STATE`, — страница, которую ему
    /// объявляет `flash_page::ErasePage` (см. `compute_ota_partitions`, пятая
    /// проверка). Меньше `PAGE_SIZE` на чипах с неравными секторами.
    pub(crate) bootloader_state_erase_size: u64,
    /// Страница обмена embassy-boot (`PAGE_SIZE`): `ACTIVE`/`DFU` кратны ей, а
    /// `DFU` длиннее `ACTIVE` ровно на неё. Больше физической страницы чипа
    /// (`MemoryLayout::page_size`), когда прошивка оборачивает `ACTIVE`/`DFU` в
    /// `flash_page::ErasePage<_, SWAP_PAGE_SIZE>` — см. [`compute_ota_partitions`].
    pub(crate) swap_page_size: u64,
    pub(crate) active_origin: u64,
    pub(crate) active_length: u64,
    pub(crate) dfu_origin: u64,
    pub(crate) dfu_length: u64,
    /// Раздел настроек, если схема считалась вместе с ним (набор `config_`):
    /// в мелких секторах перед `ACTIVE` или в хвосте flash — что оставляет
    /// `ACTIVE` больше (см. `compute_config_partition`). `None` — без настроек.
    pub(crate) settings: Option<SettingsPartition>,
}

/// Где лежит раздел настроек и какая у него страница (`flash_page::ErasePage`
/// в `bsp::flash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsPartition {
    pub(crate) origin: u64,
    pub(crate) length: u64,
    pub(crate) erase_size: u64,
}

/// Раскладка того же чипа, но с отрезанным разделом под настройки.
///
/// Отдельным набором, а не поправкой к основному: OTA-схема считается ОТ
/// размера доступного flash (`compute_ota_partitions`), поэтому при
/// уменьшении flash на две страницы `ACTIVE`/`DFU` съезжают целиком. Считать
/// это в rhai-хуке значило бы повторить там все четыре проверки
/// `assert_partitions` из `embassy-boot`.
pub(crate) struct ConfigPartition {
    /// Раздел в хвосте flash — так он лежит в проекте без OTA: таблица векторов
    /// приложения стоит в секторе 0, и раздел посреди `FLASH` разорвал бы его.
    pub(crate) tail: SettingsPartition,
    /// Что остаётся приложению, когда OTA не используется: весь flash минус
    /// раздел настроек.
    pub(crate) flash_length: u64,
    /// OTA-схема вместе с разделом; где в ней сам раздел — `settings`.
    pub(crate) ota: Result<OtaPartitions, String>,
}

/// Ядро двухъядерного H7 (STM32H745/747/755/757) — по суффиксу чип-фичи.
///
/// С заводскими option bytes (`BCM7 = BCM4 = 1`, `BOOT_CM7_ADD0 = 0x0800`,
/// `BOOT_CM4_ADD0 = 0x0810`; RM0399 §4.4.4 Table 20, §2.6 Tables 8–9, AN5557
/// §3.2) стартуют оба: CM7 — с `0x0800_0000`, CM4 — с `0x0810_0000`, начала
/// банка 2. Так их и раскладывают ST (`NUCLEO-H745ZI-Q/Templates`) и embassy
/// (`examples/stm32h755cm4`, `stm32h755cm7`): CM7 — банк 1, CM4 — банк 2.
#[derive(Clone, Copy, PartialEq, Eq)]
enum H7Core {
    /// Банк 1; первый сектор банка 2 не занимается: оттуда стартует CM4.
    Cm7,
    /// Вся раскладка — в банке 2, с точки старта CM4.
    Cm4,
}

/// Регион `DMA_NC`: non-cacheable память под буферы DMA, которую `bsp::dma_nc`
/// размечает в MPU. Без неё кэш данных ядра M7 DMA не видит.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DmaNc {
    pub(crate) origin: u64,
    pub(crate) length: u64,
}

/// Второй банк flash — куда кладётся `DFU` (см. `compute_ota_partitions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bank2 {
    origin: u64,
    length: u64,
}

/// Собирает карту памяти чипа: непрерывную flash-цепочку от базы, RAM-цепочку
/// от базы и — если помещается — партиции OTA. `None` только когда считать не
/// из чего (нет flash- или RAM-цепочки от базового адреса); тогда `main`
/// отказывается писать `chip-data/` (см. `without_layout`).
#[expect(
    clippy::too_many_lines,
    reason = "цепочки flash и RAM, резервы и наборы разделов считаются над одними и теми же регионами чипа подряд"
)]
pub(crate) fn compute_memory_layout(chip: &str, regions: &[RawRegion]) -> Option<MemoryLayout> {
    let mut flash: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| r.kind == RegionKind::Flash && r.flash.is_some())
        .collect();
    flash.sort_by_key(|r| r.address);

    let bank2 = bank2_of(regions);
    let dual_core = h7_dual_core(chip);
    // Раскладка начинается там, откуда стартует ядро: у CM4 двухъядерного H7 —
    // с банка 2 (`BOOT_CM4_ADD0`, см. `H7Core`), у остальных — с базы flash.
    let origin = match dual_core {
        Some(H7Core::Cm4) => bank2?.origin,
        _ => FLASH_BASE,
    };
    let mut chain: Vec<&RawRegion> = Vec::new();
    let mut end = origin;
    for region in flash {
        // CM7 банк 2 не продолжает: с его первого сектора стартует CM4.
        if dual_core == Some(H7Core::Cm7) && bank2.is_some_and(|b| region.address >= b.origin) {
            break;
        }
        if region.address == end {
            end += region.size;
            chain.push(region);
        } else if region.address > end {
            break;
        }
    }
    let chain_end = end;
    let flash_total = flash_for_layout(chip, chain_end - origin);
    if chain.is_empty() || flash_total == 0 {
        return None;
    }
    // Второй банк вне цепочки — между банками разрыв (H742/743/745/747 и H7A3
    // с 1 MiB: банк 2 с 0x0810_0000; U5F7/U5F9 с 2 MiB). Приложению без OTA он
    // не достаётся (раздел кода обязан быть непрерывным), а с OTA в нём лежит
    // DFU. Стыкующийся банк 2 — просто продолжение flash.
    //
    // У CM7 двухъядерного H7 разрыв — первый сектор банка 2, точка старта
    // CM4: туда не ложится ничего, и `DFU` начинается за ним. Смещения `DFU`
    // при этом всё равно от начала банка (`dfu_bank_origin` ниже) — так их
    // считает `Bank2Region`.
    let bank_gap = match dual_core {
        Some(H7Core::Cm7) => bank2.and_then(|b| {
            let boot_sector = regions
                .iter()
                .find(|r| r.kind == RegionKind::Flash && r.address == b.origin)?
                .flash?
                .erase_size;
            Some(Bank2 {
                origin: b.origin + boot_sector,
                length: b.length.checked_sub(boot_sector)?,
            })
        }),
        _ => bank2.filter(|b| b.origin >= chain_end),
    };
    // Цельный `Flash` embassy видит от базы ровно сумму банков (`FLASH_SIZE`
    // в `embassy-stm32/build.rs`), и банк 2 за разрывом ему не дотянуть. У
    // CM4 двухъядерного H7 с 2×512K туда попадает вся раскладка: писать во
    // flash (bootloader — в `ACTIVE`, приложение — в `DFU` и раздел настроек)
    // прошивке нечем, и OTA с настройками честно отключаются.
    let flash_reach = FLASH_BASE
        + regions
            .iter()
            .filter(|r| r.kind == RegionKind::Flash && r.name.starts_with("BANK_"))
            .map(|r| r.size)
            .sum::<u64>();
    let unreachable = (origin + flash_total > flash_reach).then(|| {
        format!(
            "flash ядра ({origin:#010x}..{:#010x}) за пределом цельного Flash embassy \
             (до {flash_reach:#010x}): писать туда прошивке нечем",
            origin + flash_total
        )
    });
    // PAGE_SIZE в терминах embassy-boot — не наш const generic, а
    // `max(ACTIVE::ERASE_SIZE, DFU::ERASE_SIZE)` (`boot_loader.rs`). Обе
    // партиции живут поверх цельного `Flash` (`crates-cross/boot/src/main.rs`), у
    // которого `NorFlash::ERASE_SIZE = MAX_ERASE_SIZE` всего чипа — отсюда
    // максимум по цепочке (и второму банку, если он за разрывом), а не
    // erase_size конкретного региона.
    let page_size = chain
        .iter()
        .copied()
        .chain(regions.iter().filter(|r| {
            bank_gap.is_some() && r.kind == RegionKind::Flash && r.name.starts_with("BANK_2")
        }))
        .filter_map(|r| r.flash)
        .map(|f| f.erase_size)
        .max()?;
    let write_size = chain[0].flash?.write_size;
    if page_size == 0 || write_size == 0 {
        return None;
    }
    // По первому региону цепочки, как и `write_size`: у чипа стёртое
    // состояние одно на весь контроллер флеша, а не своё у каждого банка.
    let erase_zero = chain[0].flash?.erase_value == 0;

    // RAM — цепочка с `RAM_BASE`, а у H7 — самая большая непрерывная область:
    // с `RAM_BASE` там DTCM, и в её 128 KiB статики и стек не помещаются. Кэш
    // данных M7 отладчику не мешает: probe-rs ходит через AHB-AP ядра с HPROT
    // Cacheable, и RTT из кэшируемой RAM читается верно. Остальным семействам
    // самая большая область не годится. У STM32WB SRAM2a/SRAM2b из цепочек
    // исключены (`reserved_for_cpu2`): со SRAM1 они стыкуются, и раньше RAM
    // xE/xG/xY шла через них до 0x2004_0000 — стек под flip-link и `.bss`
    // попадали в память CPU2, и прошивка падала bus error'ом ещё до `main`.
    // Ядро CM4 двухъядерных H7 берёт SRAM домена D2: AXI-SRAM занята ядром
    // CM7, а до DTCM ядро CM4 не дотягивается вовсе.
    let chains = ram_chains(chip, regions);
    let ram = if dual_core == Some(H7Core::Cm4) {
        chains
            .iter()
            .copied()
            .find(|&(start, _)| start == H7_D2_SRAM)
    } else if chip.starts_with("h7") {
        chains
            .iter()
            .copied()
            .max_by_key(|&(start, end)| (end - start, start == RAM_BASE))
    } else {
        chains.iter().copied().find(|&(start, _)| start == RAM_BASE)
    }?;
    let (ram_origin, ram_end) = ram;
    let ram_total = ram_end - ram_origin;
    if ram_total < MIN_RAM_FOR_LAYOUT {
        return None;
    }
    // Резерв отрезается от самого конца RAM — до её верхней границы; `DMA_NC`
    // (M7) занимает самый конец, а PANIC — место под ним.
    let dma_nc = dma_nc_region(chip, ram);
    let ram_tail = dma_nc.as_ref().map_or(0, |d| d.length);
    let reserved = (ram_total / PANIC_FRACTION).clamp(PANIC_MIN, PANIC_MAX);
    let ram_length = ram_total - reserved - ram_tail;
    let panic = (ram_origin + ram_length, reserved);

    Some(MemoryLayout {
        flash_origin: origin,
        flash_length: flash_total,
        ram_origin,
        ram_length,
        panic,
        // Границы цепочек, а не ram_length: PANIC отрезан от RAM, но лежит
        // внутри той же цепочки — регионом его дублировать не надо. Flash —
        // тоже до конца цепочки, а не `flash_total`: иначе у STM32WB банк,
        // обрезанный по памяти CPU2, вышел бы рабочим регионом поверх `FLASH`.
        extra_regions: extra_region_lines(chip, regions, (origin, chain_end), ram, dma_nc.as_ref()),
        write_size,
        page_size,
        erase_zero,
        // Начало банка, а не разрыва: от него `Bank2Region` считает смещения.
        dfu_bank_origin: bank_gap.and(bank2).map(|b| b.origin),
        dma_nc,
        bdma_nc: bdma_nc_region(chip, regions),
        ota: match &unreachable {
            Some(note) => Err(note.clone()),
            None => compute_ota_partitions(
                chip,
                &chain,
                flash_total,
                page_size,
                write_size,
                false,
                bank_gap,
            ),
        },
        config: if unreachable.is_some() {
            None
        } else {
            compute_config_partition(chip, &chain, flash_total, page_size, write_size, bank_gap)
        },
        external: match &unreachable {
            Some(note) => Err(note.clone()),
            None => compute_external_ota(chip, origin, flash_total, page_size),
        },
    })
}

pub(crate) fn format_addr(v: u64) -> String {
    format!("0x{v:08X}")
}

/// `K`-нотация линкер-скрипта, когда значение делится ровно — а оно всегда
/// делится: все размеры в `MemoryLayout` строятся из `erase_size`/RAM
/// регионов чипов ST, которые сами всегда кратны 1024. Запасной путь на
/// байты — просто чтобы не запаниковать, если это когда-нибудь окажется не
/// так, а не потому, что такой чип реально существует.
pub(crate) fn format_size(v: u64) -> String {
    if v.is_multiple_of(1024) {
        format!("{}K", v / 1024)
    } else {
        v.to_string()
    }
}

/// STM32WB с радиоядром CPU2 (Cortex-M0+): `wb10`…`wb55`, но не `wba` (одно
/// ядро) и не `wb0` — тот же признак, что у `mem_filter` в
/// `embassy-stm32/build.rs` («SRAM2a/SRAM2b are reserved for the radio core»).
fn has_wb_cpu2(chip: &str) -> bool {
    chip.starts_with("wb") && !chip.starts_with("wba") && !chip.starts_with("wb0")
}

/// SRAM2a/SRAM2b (и их окна I-Code) у STM32WB — память CPU2: их защищённые
/// верхние части FUS и радиостек занимают всегда, даже когда стек не
/// установлен (AN5185 §1.4–1.5, Table 5), а доступ CPU1 туда — bus error
/// (RM0434 §3.6.4). Ни в `RAM`, ни в соседние с ним регионы они не идут.
fn reserved_for_cpu2(chip: &str, region: &str) -> bool {
    has_wb_cpu2(chip) && region.starts_with("SRAM2")
}

/// Сколько flash от базы отдаётся раскладке: у STM32WB — до начала flash
/// CPU2 ([`WB_CPU2_FLASH`]), у остальных — вся цепочка.
///
/// # Panics
///
/// У STM32WB с объёмом, которого нет в таблице: границу нового чипа надо
/// взять из Release Notes копроцессора, а не угадать.
fn flash_for_layout(chip: &str, chain_length: u64) -> u64 {
    if !has_wb_cpu2(chip) {
        return chain_length;
    }
    let (_, cpu2) = WB_CPU2_FLASH
        .iter()
        .find(|(size, _)| *size == chain_length)
        .unwrap_or_else(|| {
            panic!(
                "{chip}: flash {} KiB — границы стека CPU2 для такого объёма нет в WB_CPU2_FLASH",
                chain_length / 1024
            )
        });
    cpu2 - FLASH_BASE
}

fn h7_dual_core(chip: &str) -> Option<H7Core> {
    if !chip.starts_with("h7") {
        return None;
    }
    if chip.ends_with("-cm7") {
        Some(H7Core::Cm7)
    } else if chip.ends_with("-cm4") {
        Some(H7Core::Cm4)
    } else {
        None
    }
}

/// Ядро Cortex-M7 в этом шаблоне — семейства F7 и H7 (`family_table` в
/// `chip-select.rhai`), кроме ядра CM4 двухъядерных H7 (суффикс `-cm4`).
fn is_cortex_m7(chip: &str) -> bool {
    (chip.starts_with("f7") || chip.starts_with("h7")) && !chip.ends_with("-cm4")
}

/// Наибольший размер из [`DMA_NC_SIZES`], при котором конец блока `end` кратен
/// размеру, а сам блок длиной `block` не меньше четырёх таких.
fn dma_nc_size(end: u64, block: u64) -> Option<u64> {
    DMA_NC_SIZES
        .into_iter()
        .find(|&size| block >= 4 * size && end.is_multiple_of(size))
}

/// Непрерывные цепочки RAM-блоков `(начало, конец)` по возрастанию адреса.
/// Окна внешних шин, второе (I-Code) окно блока и память CPU2 у STM32WB
/// ([`reserved_for_cpu2`]) — не память программы, в цепочки не входят; блок,
/// налезающий на цепочку, пропускается.
fn ram_chains(chip: &str, regions: &[RawRegion]) -> Vec<(u64, u64)> {
    let mut ram: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| {
            r.kind == RegionKind::Ram
                && r.size > 0
                && !r.name.ends_with(ALIAS_SUFFIX)
                && !EXTERNAL_BUS_PREFIXES.iter().any(|p| r.name.starts_with(p))
                && !reserved_for_cpu2(chip, &r.name)
        })
        .collect();
    ram.sort_by_key(|r| r.address);
    let mut chains: Vec<(u64, u64)> = Vec::new();
    for region in ram {
        match chains.last_mut() {
            Some((_, end)) if region.address == *end => *end += region.size,
            Some((_, end)) if region.address < *end => {}
            _ => chains.push((region.address, region.address + region.size)),
        }
    }
    chains
}

/// Регион `DMA_NC` у чипа с кэшем данных ядра (M7): хвост RAM, PANIC уходит
/// под него. У H7 RAM — AXI-SRAM, и регион в ней доступен всем DMA, в том
/// числе IDMA SDMMC1 из домена D1, которому SRAM домена D2 недоступна. У ядра
/// CM4 двухъядерных H7 и у не-M7 чипов региона нет. Тест
/// `every_m7_chip_gets_a_dma_region` ловит M7-чип, которому не нашлось места.
fn dma_nc_region(chip: &str, (start, end): (u64, u64)) -> Option<DmaNc> {
    if !is_cortex_m7(chip) {
        return None;
    }
    let length = dma_nc_size(end, end - start)?;
    Some(DmaNc {
        origin: end - length,
        length,
    })
}

/// `SRAM4` домена D3 у H7 целиком — память BDMA (SPI6, I2C4, LPUART1, ADC3):
/// остальные блоки ей недоступны. Кэш данных её не видит так же, как DMA1/2, поэтому
/// блок тоже non-cacheable (`bsp::dma_nc`, регион MPU по границам блока).
fn bdma_nc_region(chip: &str, regions: &[RawRegion]) -> Option<(u64, u64)> {
    if !chip.starts_with("h7") || !is_cortex_m7(chip) {
        return None;
    }
    let sram4 = regions
        .iter()
        .find(|r| r.name == "SRAM4" && r.kind == RegionKind::Ram && r.address == H7_SRAM4)?;
    (sram4.size.is_power_of_two() && sram4.size >= 32 && sram4.address.is_multiple_of(sram4.size))
        .then_some((sram4.address, sram4.size))
}

/// Второй банк по именам регионов (`BANK_2`, `BANK_2_REGION_n`): непрерывная
/// цепочка от первого из них. `None` — банк один.
///
/// Банк 2, начинающийся внутри банка 1, — не банк, а ошибка метаданных: у
/// STM32U375/U385 `stm32-data` объявляет `BANK_2` с `0x0801_0000` при `BANK_1`
/// с `0x0800_0000` на 512 KiB. Положить туда DFU значило бы писать образ поверх
/// своего же кода, поэтому такой чип считается однобанковым — как и раньше,
/// когда налезающий регион просто не попадал в цепочку flash.
fn bank2_of(regions: &[RawRegion]) -> Option<Bank2> {
    let flash = || regions.iter().filter(|r| r.kind == RegionKind::Flash);
    let bank1_end = flash()
        .filter(|r| r.name.starts_with("BANK_1"))
        .map(|r| r.address + r.size)
        .max()?;
    let mut bank: Vec<&RawRegion> = flash().filter(|r| r.name.starts_with("BANK_2")).collect();
    bank.sort_by_key(|r| r.address);
    let origin = bank.first()?.address;
    if origin < bank1_end {
        return None;
    }
    let mut end = origin;
    for region in bank {
        if region.address != end {
            break;
        }
        end += region.size;
    }
    Some(Bank2 {
        origin,
        length: end - origin,
    })
}

/// Раскладка OTA, когда `DFU` и `STATE` уезжают на внешнюю флешь.
///
/// Внутри — bootloader и `ACTIVE`, больше ничего: `ACTIVE` начинается с первой
/// границы страницы чипа после `BOOTLOADER_TARGET` (раздел над цельным
/// `Flash`, `Partition::new` требует смещения, кратного его странице) и
/// забирает всё до конца flash, сколько ложится целыми страницами обмена.
/// Bootloader не растёт сверх нужного ради `STATE` — `STATE` снаружи.
///
/// На F2/F4/F7 зона bootloader'а всё равно 128 KiB: `ACTIVE` не может начаться
/// посреди 128-килобайтного сектора, а мелкие сектора перед ним — только
/// bootloader'у.
///
/// Снаружи — `STATE`, `DFU`, раздел настроек подряд с нуля, всё по
/// `ext_align`. Четыре проверки `assert_partitions` при этом выполнены для
/// любого драйвера, чьё стирание делит `page_size`, — см.
/// `assert_external_invariants`.
fn compute_external_ota(
    chip: &str,
    flash_origin: u64,
    flash_total: u64,
    page_size: u64,
) -> Result<ExternalOta, String> {
    let page = page_size.max(EXT_ERASE_MIN);
    let zone = BOOTLOADER_TARGET.next_multiple_of(page_size);
    let active_length = flash_total.saturating_sub(zone) / page * page;
    if active_length == 0 {
        return Err(format!(
            "flash {} KiB: за bootloader'ом ({} KiB) не остаётся ни страницы обмена ({} KiB)",
            flash_total / 1024,
            zone / 1024,
            page / 1024
        ));
    }
    let ext_align = page.max(EXT_ALIGN);
    // Журнал — на самую мелкую страницу обмена, какая может выйти: драйвер со
    // стиранием мельче `page` даст `PAGE_SIZE` = странице чипа, и страниц
    // (а с ними слов журнала) станет больше.
    let journal = (2 + 4 * (active_length / page_size)) * EXT_WRITE_MAX;
    let state_length = journal.next_multiple_of(ext_align);
    let dfu_length = (active_length + page).next_multiple_of(ext_align);
    let external = ExternalOta {
        bootloader_length: zone,
        active_origin: flash_origin + zone,
        active_length,
        page_size: page,
        ext_align,
        state_offset: 0,
        state_length,
        dfu_offset: state_length,
        dfu_length,
        config_offset: state_length + dfu_length,
        config_length: 2 * ext_align,
    };
    assert_external_invariants(chip, &external, flash_origin, page_size);
    Ok(external)
}

/// Проверки `assert_partitions` и `Partition::new` для внешней раскладки — на
/// этапе генерации, по той же причине, что `assert_embassy_boot_invariants`.
/// `flash_origin` — откуда начинается раскладка (зона bootloader'а);
/// `chip_page` — крупнейший сектор чипа: страница цельного `Flash` под
/// `ACTIVE`.
fn assert_external_invariants(chip: &str, e: &ExternalOta, flash_origin: u64, chip_page: u64) {
    let page = e.page_size;
    assert!(
        page.is_multiple_of(chip_page) && page.is_multiple_of(EXT_ERASE_MIN),
        "{chip}: страница обмена {page} не кратна странице чипа или EXT_ERASE_MIN"
    );
    assert!(
        e.ext_align.is_multiple_of(page),
        "{chip}: шаг внешних разделов {} не кратен странице обмена {page}",
        e.ext_align
    );
    assert!(
        e.active_length.is_multiple_of(page) && e.dfu_length.is_multiple_of(page),
        "{chip}: ACTIVE {} или DFU {} не кратен PAGE_SIZE {page}",
        e.active_length,
        e.dfu_length
    );
    assert!(
        e.dfu_length >= e.active_length + page,
        "{chip}: DFU {} не на страницу больше ACTIVE {}",
        e.dfu_length,
        e.active_length
    );
    let journal_words = 2 + 4 * (e.active_length / chip_page);
    assert!(
        journal_words * EXT_WRITE_MAX <= e.state_length,
        "{chip}: журналу прогресса нужно {journal_words} слов по {EXT_WRITE_MAX}, в STATE {} байт",
        e.state_length
    );
    // Смещение — от базы цельного `Flash`, над которым строится раздел.
    let active_offset = e.active_origin - FLASH_BASE;
    assert!(
        active_offset.is_multiple_of(chip_page),
        "{chip}: ACTIVE не с границы страницы чипа"
    );
    assert_eq!(
        flash_origin + e.bootloader_length,
        e.active_origin,
        "{chip}: bootloader занимает всю зону до ACTIVE"
    );
    for (name, offset, length) in [
        ("STATE", e.state_offset, e.state_length),
        ("DFU", e.dfu_offset, e.dfu_length),
        ("CONFIG", e.config_offset, e.config_length),
    ] {
        assert!(
            offset.is_multiple_of(e.ext_align) && length.is_multiple_of(e.ext_align),
            "{chip}: внешний {name} (смещение {offset:#x}, размер {length}) не по шагу {}",
            e.ext_align
        );
    }
    assert!(
        e.state_offset + e.state_length <= e.dfu_offset
            && e.dfu_offset + e.dfu_length <= e.config_offset,
        "{chip}: внешние разделы накладываются"
    );
}

/// Раздел под настройки: две страницы стирания.
///
/// Две, а не одна, — требование `sequential-storage`: её `map` чередует
/// страницы, чтобы не выхаживать одну и ту же, и на одной странице просто не
/// работает («The map needs at least 2 pages to operate»). Одной и не хватило
/// бы: перед стиранием старой страницы живые записи переносятся на свободную.
///
/// Без OTA раздел — две последние страницы flash (`PAGE_SIZE`, крупнейший
/// сектор): в начале flash стоит таблица векторов приложения. С OTA есть второй
/// вариант — две соседние мелкие страницы в резерве перед `ACTIVE`, между
/// bootloader'ом и `STATE` (на F4 — сектора 2–3 по 16 KiB вместо 2×128 KiB в
/// хвосте). Берётся тот, при котором `ACTIVE` больше; при равенстве — хвост,
/// чтобы не двигать раскладку чипов с равными секторами без выигрыша.
///
/// `None` — не выделяем: либо после раздела не остаётся ни страницы под
/// приложение, либо flash не кратен странице (тогда граница раздела не легла
/// бы на сектор, а `Flash::erase` отказывает на невыровненном диапазоне).
fn compute_config_partition(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
    bank_gap: Option<Bank2>,
) -> Option<ConfigPartition> {
    let length = page_size.checked_mul(2)?;
    if !flash_total.is_multiple_of(page_size) || flash_total < length + page_size {
        return None;
    }
    let flash_length = flash_total - length;
    let tail = SettingsPartition {
        origin: chain.first()?.address + flash_length,
        length,
        erase_size: page_size,
    };
    // Раздел в хвосте: OTA-схема считается от flash, ставшего на две страницы
    // меньше.
    let behind = compute_ota_partitions(
        chip,
        chain,
        flash_length,
        page_size,
        write_size,
        false,
        bank_gap,
    )
    .map(|partitions| OtaPartitions {
        settings: Some(tail),
        ..partitions
    });
    // Раздел в резерве перед ACTIVE: схема — от всего flash.
    let ahead = compute_ota_partitions(
        chip,
        chain,
        flash_total,
        page_size,
        write_size,
        true,
        bank_gap,
    );
    let ota = match (ahead, behind) {
        (Ok(ahead), Ok(behind)) if ahead.active_length > behind.active_length => Ok(ahead),
        (Ok(ahead), Err(_)) => Ok(ahead),
        (_, behind) => behind,
    };
    Some(ConfigPartition {
        tail,
        flash_length,
        ota,
    })
}

/// Готовая строка региона для `MEMORY { }`. Ширина поля имени — 18, как у
/// строк, которые собирает сам хук (`FLASH`/`ACTIVE`/`RAM`/...), чтобы
/// сгенерированный `memory.x` выглядел одним столбиком.
fn region_line(name: &str, attrs: &str, origin: u64, length: u64) -> String {
    format!(
        "    {name:<18}{:<6}: ORIGIN = {}, LENGTH = {}",
        format!("({attrs})"),
        format_addr(origin),
        format_size(length)
    )
}

/// Все регионы памяти чипа, КРОМЕ покрытых `FLASH` и `RAM` (те строит хук:
/// у `app` и `boot` они разные). Раньше не выводились вовсе, и пользователь
/// не видел, например, 320 KiB AXISRAM у H723 — только 128 KiB DTCM.
///
/// Часть регионов выводится закомментированной (см. `EXTERNAL_BUS_PREFIXES`,
/// `ALIAS_SUFFIX`, память второго ядра — [`reserved_for_cpu2`], [`H7Core`]):
/// перечислить их надо, но объявлять рабочей памятью нельзя.
fn extra_region_lines(
    chip: &str,
    regions: &[RawRegion],
    (flash_start, flash_end): (u64, u64),
    (ram_start, ram_end): (u64, u64),
    dma_nc: Option<&DmaNc>,
) -> Vec<String> {
    let names: BTreeSet<&str> = regions.iter().map(|r| r.name.as_str()).collect();
    let covered = |r: &RawRegion| {
        let end = r.address + r.size;
        (r.address >= flash_start && end <= flash_end) || (r.address >= ram_start && end <= ram_end)
    };

    let mut sorted: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| r.size > 0 && !covered(r))
        .collect();
    sorted.sort_by_key(|r| (r.address, r.name.clone()));

    let mut lines: Vec<String> = sorted
        .iter()
        .map(|r| {
            let attrs = match r.kind {
                RegionKind::Flash => "rx",
                RegionKind::Ram => "xrw",
                // EEPROM пишется через контроллер flash, а не обычной записью,
                // поэтому без `w`: класть туда `.data` нельзя.
                RegionKind::Eeprom => "r",
            };
            let line = region_line(&r.name, attrs, r.address, r.size);

            let reason = if EXTERNAL_BUS_PREFIXES
                .iter()
                .any(|prefix| r.name.starts_with(prefix))
            {
                Some(
                    "внешняя шина: памяти нет, пока она не распаяна и не настроен контроллер"
                        .to_string(),
                )
            } else if reserved_for_cpu2(chip, &r.name) {
                Some("память CPU2 (FUS, радиостек): доступ CPU1 - bus error".to_string())
            } else if r.kind == RegionKind::Flash
                && let Some(core) = h7_dual_core(chip)
            {
                // Flash второго ядра: банк, с которого оно стартует.
                Some(
                    match core {
                        H7Core::Cm7 => "банк ядра CM4: с его первого сектора стартует CM4",
                        H7Core::Cm4 => "банк ядра CM7: с его начала стартует CM7",
                    }
                    .to_string(),
                )
            } else {
                r.name.strip_suffix(ALIAS_SUFFIX).and_then(|stem| {
                    // Партнёр — либо тот же блок без суффикса (SRAM2_ICODE /
                    // SRAM2), либо через D-Code (CCMRAM_ICODE / CCMRAM_DCODE).
                    let partner = [stem.to_string(), format!("{stem}_DCODE")]
                        .into_iter()
                        .find(|name| names.contains(name.as_str()))?;
                    Some(format!("второе окно того же блока, что {partner}"))
                })
            };

            match reason {
                Some(reason) => format!("    /* {} - {reason} */", line.trim()),
                None => line,
            }
        })
        .collect();

    // `DMA_NC` — хвост RAM, в `extra_regions` её нет; строка идёт первой, как
    // продолжение `RAM` и `PANIC`, которые хук выводит перед ними.
    if let Some(dma) = dma_nc {
        lines.insert(0, region_line("DMA_NC", "xrw", dma.origin, dma.length));
    }
    lines
}

/// Раскладка OTA со страницей обмена, при необходимости крупнее физической.
///
/// Журнал `STATE` — `(2 + 4*N) * WRITE_SIZE` байт при N страницах обмена в
/// `ACTIVE`, и на чипе с мелкой страницей и крупным flash он съедает зону
/// bootloader'а (L0: страница 128 байт, ~640 страниц — 10 KiB из 32 KiB, и
/// подписанный `boot` не влезает). Поэтому, когда журнал при физической
/// странице `page_size` больше [`STATE_BUDGET`], страница обмена удваивается
/// (`page_size * 2^k`, не больше [`SWAP_PAGE_MAX`]) до первой, при которой он
/// укладывается; прошивка оборачивает `ACTIVE`/`DFU` в `ErasePage<_, SWAP_PAGE_SIZE>`.
/// Не укладывается и на максимуме — остаётся физическая страница, а запас под
/// код держит [`BOOTLOADER_MIN`].
///
/// Расширяется только чип с равными секторами и без `DFU` в банке 2 за
/// разрывом: на F2/F4/F7 страница обмена и так крупнейший сектор, а банк 2 за
/// разрывом лежит в другом регионе `Flash`, и единая страница на оба раздела
/// не получилась бы.
fn compute_ota_partitions(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
    with_settings: bool,
    bank_gap: Option<Bank2>,
) -> Result<OtaPartitions, String> {
    let at = |swap| {
        compute_ota_at(
            chip,
            chain,
            flash_total,
            swap,
            write_size,
            with_settings,
            bank_gap,
        )
    };
    let fits = |p: &OtaPartitions| journal_bytes(p, write_size) <= STATE_BUDGET;
    let base = at(page_size);
    let uniform = bank_gap.is_none()
        && chain
            .iter()
            .all(|r| r.flash.is_some_and(|f| f.erase_size == page_size));
    if !uniform || base.as_ref().is_ok_and(fits) {
        return base;
    }
    let mut swap = page_size;
    while swap < SWAP_PAGE_MAX {
        swap *= 2;
        if let Ok(partitions) = at(swap)
            && fits(&partitions)
        {
            return Ok(partitions);
        }
    }
    base
}

/// Крупнейший сектор стирания по цепочке flash: страница цельного `Flash`.
fn chain_page(chain: &[&RawRegion]) -> u64 {
    chain
        .iter()
        .filter_map(|r| r.flash)
        .map(|f| f.erase_size)
        .max()
        .unwrap_or(1)
}

/// Журнал обмена `embassy-boot` в байтах: слово записи на страницу `ACTIVE` в
/// каждом из четырёх проходов плюс два служебных (проверка 4).
fn journal_bytes(p: &OtaPartitions, write_size: u64) -> u64 {
    (2 + 4 * (p.active_length / p.swap_page_size)) * write_size
}

/// Подбирает `BOOTLOADER`/`BOOTLOADER_STATE`/`ACTIVE`/`DFU` для страницы обмена
/// `page_size` (физической или расширенной, см. [`compute_ota_partitions`]; вся
/// арифметика ниже — в её единицах) под все четыре
/// проверки `assert_partitions` (`embassy-boot`, `boot_loader.rs`) — это
/// runtime-assert'ы, а не документация: не выполнены — bootloader паникует
/// на старте, а не отказывается собираться.
///
/// 1. `ACTIVE` кратен `PAGE_SIZE`;
/// 2. `DFU` кратен `PAGE_SIZE`;
/// 3. `DFU - ACTIVE >= PAGE_SIZE` — запас алгоритма swap;
/// 4. `2 + 4 * (ACTIVE / PAGE_SIZE) <= STATE / NorFlash::WRITE_SIZE` — в
///    `BOOTLOADER_STATE` лежит журнал прогресса swap: по слову на страницу
///    ACTIVE в каждом из четырёх проходов плюс два служебных.
///
/// Четвёртая и есть причина, по которой `BOOTLOADER_STATE` нельзя делать
/// «просто одним сектором»: на чипе с мелкими секторами и крупным flash
/// (L4, 1 MiB при секторе 2 KiB) журналу нужно `(2 + 4*247) * 8` байт, то
/// есть четыре сектора, а не один. Раньше здесь всегда стоял ровно один
/// сектор — и 279 из 972 посчитанных чипов давали bootloader, падающий в
/// панику при первом же запуске.
///
/// Сам `BOOTLOADER` и `BOOTLOADER_STATE` кратности `PAGE_SIZE` не подчиняются
/// (в `assert_partitions` их нет) и режутся по локальной границе сектора —
/// иначе на F4/F7/H7 (сектор до 128 KiB) один отступ съедал бы четверть чипа.
///
/// Но есть пятая проверка, не из `assert_partitions`: раздел строится
/// `Partition::new` из `embassy-embedded-hal`, а тот паникует, если смещение
/// или размер не кратны `NorFlash::ERASE_SIZE` флеша под разделом. У цельного
/// `Flash` это самый крупный сектор, и на F2/F4/F7 `STATE` в мелком секторе
/// (0x10000 при странице 128 KiB у F4) ронял и bootloader, и приложение на
/// первом же старте — у всех 223 таких чипов с OTA. Поэтому `STATE` строится
/// поверх `flash_page::ErasePage` со страницей своего сектора, и размер этого
/// сектора — `bootloader_state_erase_size` — уходит в шаблон вместе с адресами.
/// Сам сектор — самый мелкий в резерве, см. `place_state`.
///
/// Обычно `DFU` идёт сразу за `ACTIVE`, а остаток flash делится между ними
/// пополам — так прошивка выходит наибольшей. Банки, которые стыкуются, для
/// этого — один непрерывный flash. Если же банк 2 стоит за разрывом
/// (`bank_gap`; `flash_total` тогда кончается на банке 1), кода в нём быть не
/// может, и он целиком отдаётся под `DFU`: `ACTIVE` — всё, что осталось от
/// банка 1 после резерва (но не больше, чем позволяет банк 2), `DFU` — с
/// начала банка 2. Пишет его прошивка через регион банка
/// (`MemoryLayout::dfu_bank_origin`).
///
/// `Err` — схема не помещается; текст объясняет, чего именно не хватило, и
/// доходит до пользователя при генерации.
#[expect(
    clippy::too_many_lines,
    reason = "проверки схемы идут по порядку и каждая объясняет свой отказ; разнесённые по функциям, они потеряли бы общий контекст сообщений"
)]
fn compute_ota_at(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
    with_settings: bool,
    bank_gap: Option<Bank2>,
) -> Result<OtaPartitions, String> {
    // Банк 2 за разрывом — весь под `DFU`.
    let dfu_zone = bank_gap.map(|b| (b.origin, b.origin + b.length));
    // Начало раскладки: база flash, а у CM4 двухъядерного H7 — банк 2. Все
    // смещения ниже — от него.
    let origin = chain
        .first()
        .ok_or_else(|| "flash-цепочка пуста".to_string())?
        .address;
    let erase_size_at = |offset_from_origin: u64| -> Option<u64> {
        let addr = origin + offset_from_origin;
        chain
            .iter()
            .find(|r| addr >= r.address && addr < r.address + r.size)
            .and_then(|r| r.flash)
            .map(|f| f.erase_size)
    };
    // Размеры секторов бывают и меньше килобайта (L0/L1 — 256 байт), поэтому
    // не просто деление на 1024: иначе в сообщении о причине оказался бы
    // "сектор 0 KiB".
    let size = |v: u64| {
        if v >= 1024 {
            format!("{} KiB", v / 1024)
        } else {
            format!("{v} B")
        }
    };

    let initial_pages = BOOTLOADER_TARGET.div_ceil(page_size).max(1);
    let max_extra_pages =
        MAX_EXTRA_RESERVED_PAGES.max(MAX_EXTRA_RESERVED_BYTES.div_ceil(page_size));
    let mut reserved_pages = initial_pages;
    loop {
        if reserved_pages > initial_pages + max_extra_pages {
            return Err(format!(
                "резерв под bootloader не подобрался, дорастив его на {max_extra_pages} страниц по {}",
                size(page_size)
            ));
        }
        let reserved_size = reserved_pages * page_size;
        if reserved_size >= flash_total {
            return Err(format!(
                "flash {} меньше резерва под bootloader ({} при секторе {})",
                size(flash_total),
                size(reserved_size),
                size(page_size)
            ));
        }
        let Some(local_erase) = erase_size_at(reserved_size - 1).filter(|erase| *erase > 0) else {
            return Err("не удалось определить размер сектора на границе резерва".to_string());
        };
        if !reserved_size.is_multiple_of(local_erase) {
            // На реальных чипах ST не встречается (erase_size регионов чипа
            // кратны друг другу), но битый линкер-скрипт хуже честного отказа.
            return Err(format!(
                "резерв {} не ложится на границу сектора {}",
                size(reserved_size),
                size(local_erase)
            ));
        }

        let active_room = (flash_total - reserved_size) / page_size;
        let (active_pages, dfu_pages, dfu_origin) = if let Some((origin, end)) = dfu_zone {
            // DFU обязан быть на страницу больше ACTIVE (проверка 3), так что
            // ACTIVE упирается в меньшее из двух: остаток банка 1 и банк 2 без
            // страницы.
            let dfu_room = (end - origin) / page_size;
            let active_pages = active_room.min(dfu_room.saturating_sub(1));
            if active_pages == 0 {
                return Err(format!(
                    "двухбанковый flash при секторе {}: под BOOTLOADER+STATE уходит {}, \
                     в банке 1 под ACTIVE остаётся страниц: {active_room}, в банке 2 под DFU — \
                     {dfu_room} (нужно хотя бы 1 и 2)",
                    size(page_size),
                    size(reserved_size)
                ));
            }
            (active_pages, active_pages + 1, origin)
        } else {
            let remaining_pages = active_room;
            if remaining_pages < 3 {
                // Дальше растить резерв бессмысленно — остаток только уменьшится.
                return Err(format!(
                    "flash {} при секторе {}: под BOOTLOADER+STATE уходит {}, \
                     на ACTIVE+DFU остаётся стираемых страниц: {remaining_pages} (нужно 3)",
                    size(flash_total),
                    size(page_size),
                    size(reserved_size)
                ));
            }
            // Пополам остаток не делится: DFU обязан быть на страницу больше
            // ACTIVE (проверка 3).
            let active_pages = (remaining_pages - 1) / 2;
            let dfu_start = origin + reserved_size + active_pages * page_size;
            (active_pages, remaining_pages - active_pages, dfu_start)
        };

        // Проверка 4, решённая относительно STATE: сколько журнала нужно под
        // такой ACTIVE.
        let state_needed = (2 + 4 * active_pages) * write_size;
        let placed = reserve_sectors(&erase_size_at, reserved_size).and_then(|sectors| {
            if with_settings {
                place_settings_and_state(origin, &sectors, state_needed)
                    .map(|(settings, state)| (Some(settings), state))
            } else {
                place_state(&sectors, state_needed, BOOTLOADER_MIN).map(|state| (None, state))
            }
        });
        let Some((settings, (state_offset, state_length, state_erase))) = placed else {
            // Журнал (и раздел настроек) не легли в сектора резерва, не отняв у
            // bootloader'а минимума, — помогает резерв крупнее: в нём больше
            // секторов, а ACTIVE, а с ним и журнал, меньше.
            reserved_pages += 1;
            continue;
        };

        let active_origin = origin + reserved_size;
        let active_length = active_pages * page_size;
        let partitions = OtaPartitions {
            bootloader_length: settings.map_or(state_offset, |s| s.origin - origin),
            bootloader_state_origin: origin + state_offset,
            bootloader_state_length: state_length,
            bootloader_state_erase_size: state_erase,
            swap_page_size: page_size,
            active_origin,
            active_length,
            dfu_origin,
            dfu_length: dfu_pages * page_size,
            settings,
        };
        assert_embassy_boot_invariants(chip, &partitions, origin, chain_page(chain), write_size);
        return Ok(partitions);
    }
}

/// Где в резерве `[0, reserved_size)` лежит `BOOTLOADER_STATE`: смещение,
/// длина и размер его сектора. `None` — не лёг.
///
/// В самых мелких секторах резерва и как можно ближе к его концу. Журналу
/// нужны десятки байт, а стирается раздел при каждом обновлении, так что
/// мелкий сектор и места не отнимает, и стирается быстрее. Всё, что перед
/// ним, — bootloader, и он обязан получить хотя бы `BOOTLOADER_MIN`. Что
/// остаётся между `STATE` и `ACTIVE`, не занято: на F4 это 64-килобайтный
/// сектор 4 — под `ACTIVE` он не годится (страница там 128 KiB), а `STATE`
/// ему мельче предпочтителен.
///
/// На чипе с равными секторами «самые мелкие» — все, «ближе к концу» — вплотную
/// к `ACTIVE`, то есть раскладка та же, что до этого правила.
///
/// Если мелкие сектора стоят так близко к началу, что bootloader'у перед ними
/// не хватает минимума, пробуется следующий размер — а не сразу резерв крупнее:
/// тот не поменял бы, где лежат мелкие сектора.
///
/// `sectors` — `(смещение, размер)` секторов, где можно искать; `min_offset` —
/// раньше какого смещения `STATE` начинаться не может (перед ним bootloader или
/// раздел настроек).
fn place_state(
    sectors: &[(u64, u64)],
    state_needed: u64,
    min_offset: u64,
) -> Option<(u64, u64, u64)> {
    let sizes: BTreeSet<u64> = sectors.iter().map(|(_, erase)| *erase).collect();
    sizes.into_iter().find_map(|size| {
        let count = usize::try_from(state_needed.div_ceil(size)).ok()?;
        // Окно из `count` подряд идущих секторов этого размера — от последнего
        // к первому.
        sectors
            .windows(count)
            .rev()
            .find(|window| window.iter().all(|(_, erase)| *erase == size))
            .map(|window| (window[0].0, size * count as u64, size))
            .filter(|(state_offset, _, _)| *state_offset >= min_offset)
    })
}

/// Сектора резерва `[0, reserved_size)` по порядку: `(смещение, размер)`.
/// `None` — на каком-то смещении размер сектора неизвестен.
fn reserve_sectors(
    erase_size_at: &impl Fn(u64) -> Option<u64>,
    reserved_size: u64,
) -> Option<Vec<(u64, u64)>> {
    let mut sectors = Vec::new();
    let mut offset = 0;
    while offset < reserved_size {
        let erase = erase_size_at(offset).filter(|erase| *erase > 0)?;
        sectors.push((offset, erase));
        offset += erase;
    }
    Some(sectors)
}

/// Раздел настроек и `STATE` в резерве: bootloader, за ним настройки — две
/// соседние страницы одного размера, — за ними `STATE`. `None` — не легли.
///
/// Настройки — как можно ближе к концу резерва, но так, чтобы после них нашлось
/// место под `STATE`: на F4 это сектора 2–3 (2×16 KiB), `STATE` — сектор 4
/// (64 KiB), bootloader — 32 KiB. `STATE` здесь, в отличие от проекта без
/// настроек, может попасть в сектор крупнее: журналу нужны десятки байт и
/// размер сектора ему всё равно, а настройкам нужны две равные страницы, и
/// мелкие сектора отдаются им.
///
/// Bootloader перед разделом получает не меньше `BOOTLOADER_TARGET`, а не
/// `BOOTLOADER_MIN`, как без настроек: раздел в резерве — это выгода для
/// `ACTIVE`, и оплачивать её бинарником bootloader'а нельзя. Иначе на чипах с
/// равными секторами раздел садился бы в запас bootloader'а и ужимал его до
/// 16 KiB ради одной-двух страниц `ACTIVE`; с этой границей такие чипы честно
/// получают раздел в хвосте (см. `compute_config_partition`).
///
/// Смещения `sectors` — от `origin`, начала раскладки.
fn place_settings_and_state(
    origin: u64,
    sectors: &[(u64, u64)],
    state_needed: u64,
) -> Option<(SettingsPartition, (u64, u64, u64))> {
    (0..sectors.len().saturating_sub(1)).rev().find_map(|i| {
        let (offset, erase) = sectors[i];
        if sectors[i + 1].1 != erase || offset < BOOTLOADER_TARGET {
            return None;
        }
        let state = place_state(&sectors[i + 2..], state_needed, 0)?;
        Some((
            SettingsPartition {
                origin: origin + offset,
                length: 2 * erase,
                erase_size: erase,
            },
            state,
        ))
    })
}

/// Те же четыре проверки, что `assert_partitions` делает в рантайме на плате, и
/// пятая — от `Partition::new` (кратность смещения и размера раздела странице
/// флеша под ним), — но здесь, на этапе генерации. Страница обмена —
/// `p.swap_page_size`: кратна `chip_page` (крупнейший сектор чипа), и по ней
/// же сверяются `ACTIVE`/`DFU` (при расширенной странице прошивка строит их
/// поверх `ErasePage` с этой страницей). Паника вместо `Err`: `compute_ota_partitions`
/// обязан выдавать только валидные раскладки, невалидная — баг в нём самом, а
/// не «чип не подошёл». Ровно этой проверки не хватало: 279 из 972 прежних
/// раскладок нарушали четвёртую и роняли bootloader на первом же старте.
///
/// `flash_origin` — начало зоны bootloader'а; смещения для `Partition::new`
/// — от базы цельного `Flash` (`FLASH_BASE`), как их считает прошивка.
fn assert_embassy_boot_invariants(
    chip: &str,
    p: &OtaPartitions,
    flash_origin: u64,
    chip_page: u64,
    write_size: u64,
) {
    let page_size = p.swap_page_size;
    assert!(
        page_size.is_multiple_of(chip_page),
        "{chip}: страница обмена {page_size} не кратна странице чипа {chip_page}"
    );
    assert!(
        p.active_length.is_multiple_of(page_size),
        "{chip}: ACTIVE {} не кратен PAGE_SIZE {page_size}",
        p.active_length
    );
    assert!(
        p.dfu_length.is_multiple_of(page_size),
        "{chip}: DFU {} не кратен PAGE_SIZE {page_size}",
        p.dfu_length
    );
    assert!(
        p.dfu_length >= p.active_length + page_size,
        "{chip}: DFU {} не на страницу больше ACTIVE {}",
        p.dfu_length,
        p.active_length
    );
    let journal_words = 2 + 4 * (p.active_length / page_size);
    let state_words = p.bootloader_state_length / write_size;
    assert!(
        journal_words <= state_words,
        "{chip}: журналу прогресса нужно {journal_words} слов, в BOOTLOADER_STATE помещается {state_words}"
    );
    // Порядок: bootloader, [настройки,] STATE, ACTIVE, DFU — и настройки в
    // хвосте, если не в резерве. Между STATE и ACTIVE может остаться незанятый
    // сектор (см. `place_state`), между ACTIVE и DFU — остаток банка 1 у
    // двухбанкового чипа; хвостовые настройки лежат после ACTIVE и не задевают
    // DFU (за ним — на непрерывном flash, перед ним — у банка 2 за разрывом).
    let ahead = p.settings.filter(|s| s.origin < p.active_origin);
    let after_bootloader = ahead.map_or(p.bootloader_state_origin, |s| s.origin);
    let active_end = p.active_origin + p.active_length;
    let dfu_end = p.dfu_origin + p.dfu_length;
    assert!(
        after_bootloader == flash_origin + p.bootloader_length
            && ahead.is_none_or(|s| s.origin + s.length <= p.bootloader_state_origin)
            && p.bootloader_state_origin + p.bootloader_state_length <= p.active_origin
            && p.dfu_origin >= active_end
            && p.settings.is_none_or(|s| {
                ahead.is_some()
                    || (s.origin >= active_end
                        && (s.origin + s.length <= p.dfu_origin || s.origin >= dfu_end))
            }),
        "{chip}: партиции накладываются или идут не по порядку"
    );
    if let Some(s) = p.settings {
        assert!(
            s.length == 2 * s.erase_size,
            "{chip}: раздел настроек {} — не две страницы по {}",
            s.length,
            s.erase_size
        );
    }
    // `ACTIVE`/`DFU` строятся поверх цельного `Flash` либо `ErasePage` со страницей
    // обмена, `STATE` — поверх `ErasePage` со страницей своего сектора.
    let state_offset = p.bootloader_state_origin - FLASH_BASE;
    for (name, offset, length, page) in [
        (
            "ACTIVE",
            p.active_origin - FLASH_BASE,
            p.active_length,
            page_size,
        ),
        ("DFU", p.dfu_origin - FLASH_BASE, p.dfu_length, page_size),
        (
            "BOOTLOADER_STATE",
            state_offset,
            p.bootloader_state_length,
            p.bootloader_state_erase_size,
        ),
    ]
    .into_iter()
    .chain(
        p.settings
            .map(|s| ("CONFIG", s.origin - FLASH_BASE, s.length, s.erase_size)),
    ) {
        assert!(
            offset.is_multiple_of(page) && length.is_multiple_of(page),
            "{chip}: {name} (смещение {offset:#x}, размер {length}) не кратен своей странице {page}: Partition::new запаникует"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::FlashSettings;

    /// Точка старта CM4 с заводскими option bytes — начало банка 2.
    const H7_BANK2: u64 = 0x0810_0000;

    /// Первый сектор банка 2: с него стартует CM4, ядру CM7 туда нельзя.
    const CM4_BOOT_SECTOR: std::ops::Range<u64> = H7_BANK2..H7_BANK2 + 128 * 1024;

    /// Флеш из двух банков по `bank` байт со страницей 2 KiB; второй — с
    /// адреса `bank2_origin` (вплотную к первому или за разрывом, как у G47x
    /// с 128/256 KiB).
    fn two_banks(bank: u64, bank2_origin: u64) -> Vec<RawRegion> {
        let mut regions = uniform_flash(bank, 2048, 8);
        regions.push(RawRegion {
            name: "BANK_2".to_string(),
            kind: RegionKind::Flash,
            address: bank2_origin,
            size: bank,
            flash: regions[0].flash,
        });
        regions
    }

    /// Банки стыкуются (H743ZI, L476): это один непрерывный flash, и ACTIVE с
    /// DFU делят его пополам, как у однобанкового, — DFU не привязан к банку 2.
    #[test]
    fn contiguous_banks_are_one_flash() {
        let bank = 256 * 1024;
        let layout =
            compute_memory_layout("test", &two_banks(bank, FLASH_BASE + bank)).expect("раскладка");
        assert_eq!(layout.flash_length, 2 * bank);
        assert_eq!(layout.dfu_bank_origin, None);
        let ota = layout.ota.expect("OTA помещается");
        assert_eq!(ota.dfu_origin, ota.active_origin + ota.active_length);
        assert!(ota.active_origin + ota.active_length > FLASH_BASE + bank);
    }

    /// Банк 2 за разрывом (как у H743 с 1 MiB: банк 2 с `0x0810_0000`): цепочка
    /// для приложения — только банк 1, DFU — в банке 2, и хук узнаёт об этом
    /// по `dfu_bank_origin`.
    #[test]
    fn bank_2_behind_a_gap_still_takes_the_dfu() {
        let bank = 64 * 1024;
        let layout = compute_memory_layout("test", &two_banks(bank, FLASH_BASE + 0x4_0000))
            .expect("раскладка");
        assert_eq!(layout.flash_length, bank);
        assert_eq!(layout.dfu_bank_origin, Some(FLASH_BASE + 0x4_0000));
        let ota = layout.ota.expect("OTA помещается");
        assert_eq!(ota.dfu_origin, FLASH_BASE + 0x4_0000);
        assert!(ota.dfu_origin + ota.dfu_length <= FLASH_BASE + 0x4_0000 + bank);
        assert_eq!(ota.dfu_length, ota.active_length + 2048);

        // Настройки в хвосте — в конце банка 1, между ACTIVE и разрывом.
        let with_config = layout
            .config
            .expect("раздел")
            .ota
            .expect("OTA с настройками");
        let settings = with_config.settings.expect("раздел");
        assert!(settings.origin >= with_config.active_origin + with_config.active_length);
        assert_eq!(settings.origin + settings.length, FLASH_BASE + bank);
    }

    /// Один flash-регион, `size`/`erase_size` в байтах, RAM фиксирована в
    /// 64 KiB. Стирание в `0xFF` — как у подавляющего большинства семейств;
    /// обратный случай собирает [`flash_erasing_to_zero`].
    fn uniform_flash(size: u64, erase_size: u64, write_size: u64) -> Vec<RawRegion> {
        uniform_flash_with_ram(size, erase_size, write_size, 64 * 1024)
    }

    /// То же самое, но с произвольным размером RAM — нужен там, где важен
    /// именно он (например, границы `PANIC`), а не фиксированные 64 KiB.
    fn uniform_flash_with_ram(
        size: u64,
        erase_size: u64,
        write_size: u64,
        ram: u64,
    ) -> Vec<RawRegion> {
        let mut regions = flash_with_erase_value(size, erase_size, write_size, 0xFF);
        let sram = regions
            .iter_mut()
            .find(|r| r.kind == RegionKind::Ram)
            .expect("flash_with_erase_value всегда добавляет регион RAM");
        sram.size = ram;
        regions
    }

    /// Флеш, стирающийся в ноль (L0/L1, STM32WB10CC/WB15CC).
    fn flash_erasing_to_zero(size: u64, erase_size: u64, write_size: u64) -> Vec<RawRegion> {
        flash_with_erase_value(size, erase_size, write_size, 0x00)
    }

    fn flash_with_erase_value(
        size: u64,
        erase_size: u64,
        write_size: u64,
        erase_value: u8,
    ) -> Vec<RawRegion> {
        vec![
            RawRegion {
                name: "BANK_1".to_string(),
                kind: RegionKind::Flash,
                address: FLASH_BASE,
                size,
                flash: Some(FlashSettings {
                    erase_size,
                    write_size,
                    erase_value,
                }),
            },
            RawRegion {
                name: "SRAM".to_string(),
                kind: RegionKind::Ram,
                address: RAM_BASE,
                size: 64 * 1024,
                flash: None,
            },
        ]
    }

    /// Стёртое состояние берётся из метаданных, а не из имени чипа.
    ///
    /// Пока признак считался как «фича начинается на `l0`/`l1`», мимо него
    /// проходили STM32WB10CC/WB15CC: `erase_value: 0` при другом префиксе.
    /// Такому чипу доставался `embassy-boot` без `flash-erase-zero` (обмен
    /// разделов читает признак «стёрто» наоборот) и раздел настроек, который
    /// не работает вовсе, — обе поломки молчаливые.
    #[test]
    fn erase_zero_comes_from_the_metadata() {
        let ordinary = compute_memory_layout("test", &uniform_flash(512 * 1024, 2048, 8))
            .expect("раскладка для обычного флеша");
        assert!(!ordinary.erase_zero, "0xFF принят за ноль");

        let zeroing = compute_memory_layout("test", &flash_erasing_to_zero(512 * 1024, 2048, 8))
            .expect("раскладка для флеша, стирающегося в ноль");
        assert!(zeroing.erase_zero, "0x00 не распознан");
    }

    #[test]
    fn state_grows_with_active_to_fit_the_swap_journal() {
        // Страница обмена, не расширенная (физическая): журнал растёт с ACTIVE.
        // L476RG: 1 MiB при секторе 2 KiB, журналу нужно (2 + 4*247) слов по
        // 8 байт — одного сектора не хватает (раньше стоял ровно один, и
        // bootloader падал в панику на старте).
        let chain = uniform_flash(1024 * 1024, 2048, 8);
        let chain: Vec<&RawRegion> = chain
            .iter()
            .filter(|r| r.kind == RegionKind::Flash)
            .collect();
        let ota = compute_ota_at("test", &chain, 1024 * 1024, 2048, 8, false, None)
            .expect("OTA должна помещаться");
        assert!(ota.bootloader_state_length > 2048);
        assert_eq!(ota.bootloader_state_length, 8192);
    }

    /// Журнал не умещается в `STATE_BUDGET` — страница обмена вырастает, и
    /// `STATE` сжимается до сотен байт. 512 KiB при странице 2 KiB: 2 -> 4 KiB
    /// (на 2 KiB журналу нужно 3792 байта); L476RG с 1 MiB упирается в 8 KiB.
    #[test]
    fn swap_page_grows_until_the_journal_fits_the_budget() {
        let big = compute_memory_layout("test", &uniform_flash(1024 * 1024, 2048, 8))
            .expect("раскладка должна посчитаться");
        assert_eq!(big.ota.expect("OTA должна помещаться").swap_page_size, 8192);

        let layout = compute_memory_layout("test", &uniform_flash(512 * 1024, 2048, 8))
            .expect("раскладка должна посчитаться");
        assert_eq!(layout.page_size, 2048);
        let ota = layout.ota.expect("OTA должна помещаться");
        assert_eq!(ota.swap_page_size, 4096);
        assert!(journal_bytes(&ota, 8) <= STATE_BUDGET);
        assert!(ota.bootloader_state_length <= STATE_BUDGET);
        assert!(ota.bootloader_length >= BOOTLOADER_MIN);
        assert!(ota.active_origin.is_multiple_of(4096));
        assert!(ota.dfu_length >= ota.active_length + 4096 && ota.dfu_length.is_multiple_of(4096));
    }

    /// L073RZ: страница 128 байт, 192 KiB flash — без расширения `STATE` 10 KiB.
    #[test]
    fn tiny_pages_get_a_wide_swap_page_and_a_big_bootloader_zone() {
        let layout = compute_memory_layout("test", &flash_erasing_to_zero(192 * 1024, 128, 4))
            .expect("раскладка должна посчитаться");
        let ota = layout.ota.expect("OTA должна помещаться");
        assert!(ota.swap_page_size > 128 && ota.swap_page_size <= SWAP_PAGE_MAX);
        assert!(ota.bootloader_length >= BOOTLOADER_MIN);
        assert!(ota.bootloader_state_length <= STATE_BUDGET);
        assert_eq!(ota.bootloader_state_erase_size, 128);
    }

    /// Небольшой flash: журнал и так в бюджете, страница не трогается.
    #[test]
    fn swap_page_stays_physical_when_the_journal_already_fits() {
        let layout =
            compute_memory_layout("test", &uniform_flash(128 * 1024, 2048, 8)).expect("раскладка");
        let ota = layout.ota.expect("OTA помещается");
        assert_eq!(ota.swap_page_size, 2048);
        assert!(ota.bootloader_length >= BOOTLOADER_MIN);
    }

    /// F4 (неравные сектора) и банк 2 за разрывом страницу не расширяют.
    #[test]
    fn swap_page_is_not_widened_on_unequal_sectors_or_behind_a_bank_gap() {
        let f4 = compute_memory_layout("test", &f407ve()).expect("раскладка");
        assert_eq!(f4.ota.expect("OTA").swap_page_size, f4.page_size);
        assert_eq!(
            f4.config
                .expect("настройки")
                .ota
                .expect("OTA")
                .swap_page_size,
            f4.page_size
        );

        let gap = compute_memory_layout("test", &two_banks(64 * 1024, FLASH_BASE + 0x4_0000))
            .expect("раскладка");
        assert_eq!(gap.ota.expect("OTA").swap_page_size, gap.page_size);
    }

    /// Карта F407VE: 4×16 KiB, 64 KiB, дальше по 128 KiB. `STATE` — в
    /// последнем 16-килобайтном секторе (журналу хватает с запасом), перед ним
    /// bootloader, сектор 4 пустует. Со страницей всего чипа (128 KiB)
    /// `Partition::new` на таком разделе паниковал, поэтому раскладка обязана
    /// отдать страницу его собственного сектора.
    /// Карта F407VE: 4×16 KiB, 64 KiB, дальше 3×128 KiB.
    fn f407ve() -> Vec<RawRegion> {
        let sector = |name: &str, address: u64, size: u64, erase_size: u64| RawRegion {
            name: name.to_string(),
            kind: RegionKind::Flash,
            address,
            size,
            flash: Some(FlashSettings {
                erase_size,
                write_size: 4,
                erase_value: 0xFF,
            }),
        };
        let mut regions = uniform_flash(0, 1, 4);
        regions.retain(|r| r.kind == RegionKind::Ram);
        regions.extend([
            sector("BANK_1_REGION_1", FLASH_BASE, 64 * 1024, 16 * 1024),
            sector(
                "BANK_1_REGION_2",
                FLASH_BASE + 0x1_0000,
                64 * 1024,
                64 * 1024,
            ),
            sector(
                "BANK_1_REGION_3",
                FLASH_BASE + 0x2_0000,
                384 * 1024,
                128 * 1024,
            ),
        ]);
        regions
    }

    #[test]
    fn state_takes_the_last_smallest_sector_with_its_own_page() {
        let layout =
            compute_memory_layout("test", &f407ve()).expect("раскладка должна посчитаться");
        assert_eq!(layout.page_size, 128 * 1024);
        let ota = layout.ota.expect("OTA должна помещаться");
        assert_eq!(ota.bootloader_length, 48 * 1024);
        assert_eq!(ota.bootloader_state_origin, FLASH_BASE + 0xC000);
        assert_eq!(ota.bootloader_state_length, 16 * 1024);
        assert_eq!(ota.bootloader_state_erase_size, 16 * 1024);
        assert_eq!(ota.active_origin, FLASH_BASE + 0x2_0000);

        // С разделом настроек — вариант A: bootloader 0–1, настройки 2–3
        // (2×16 KiB), STATE в секторе 4. ACTIVE тот же, что без настроек, а в
        // хвосте раздел стоил бы 2×128 KiB и вытеснил бы OTA вовсе.
        let config = layout.config.expect("раздел настроек возможен");
        let ota = config.ota.expect("OTA с настройками должна помещаться");
        assert_eq!(ota.bootloader_length, 32 * 1024);
        assert_eq!(
            ota.settings,
            Some(SettingsPartition {
                origin: FLASH_BASE + 0x8000,
                length: 32 * 1024,
                erase_size: 16 * 1024,
            })
        );
        assert_eq!(ota.bootloader_state_origin, FLASH_BASE + 0x1_0000);
        assert_eq!(ota.bootloader_state_erase_size, 64 * 1024);
        assert_eq!(ota.active_origin, FLASH_BASE + 0x2_0000);
        assert_eq!(ota.active_length, 128 * 1024);
        // Без OTA раздел по-прежнему в хвосте.
        assert_eq!(config.tail.origin, FLASH_BASE + 0x4_0000);
        assert_eq!(config.tail.erase_size, 128 * 1024);
    }

    /// Пятая проверка — от `Partition::new`: раздел, не кратный странице флеша
    /// под ним, ловится при генерации, а не паникой на плате.
    #[test]
    #[should_panic(expected = "Partition::new")]
    fn state_misaligned_to_its_page_is_caught() {
        let state_on_the_chip_page = OtaPartitions {
            bootloader_length: 64 * 1024,
            bootloader_state_origin: FLASH_BASE + 0x1_0000,
            bootloader_state_length: 64 * 1024,
            bootloader_state_erase_size: 128 * 1024,
            swap_page_size: 128 * 1024,
            active_origin: FLASH_BASE + 0x2_0000,
            active_length: 128 * 1024,
            dfu_origin: FLASH_BASE + 0x4_0000,
            dfu_length: 256 * 1024,
            settings: None,
        };
        assert_embassy_boot_invariants("test", &state_on_the_chip_page, FLASH_BASE, 128 * 1024, 4);
    }

    /// Мелкие сектора у самого начала (4×4 KiB): bootloader'у перед последним
    /// из них досталось бы 12 KiB, меньше `BOOTLOADER_MIN`. Тогда `STATE` —
    /// в следующем по размеру секторе, а не отказ от OTA.
    #[test]
    fn state_falls_back_to_a_larger_sector_when_small_ones_are_too_early() {
        let sector = |offset: u64| -> Option<u64> {
            Some(if offset < 16 * 1024 {
                4 * 1024
            } else {
                64 * 1024
            })
        };
        let sectors = reserve_sectors(&sector, 144 * 1024).expect("сектора известны");
        assert_eq!(
            place_state(&sectors, 64, BOOTLOADER_MIN),
            Some((80 * 1024, 64 * 1024, 64 * 1024))
        );
    }

    /// Внешняя раскладка на F407VE (4×16, 64, 3×128 KiB): bootloader — до
    /// первой 128-килобайтной границы, `ACTIVE` — всё остальное, снаружи
    /// `STATE`, `DFU`, настройки по 128 KiB (страница обмена крупнее блока).
    #[test]
    fn external_layout_keeps_everything_but_the_bootloader_for_active() {
        let e =
            compute_external_ota("test", FLASH_BASE, 512 * 1024, 128 * 1024).expect("помещается");
        assert_eq!(e.bootloader_length, 128 * 1024);
        assert_eq!(e.active_origin, FLASH_BASE + 0x2_0000);
        assert_eq!(e.active_length, 384 * 1024);
        assert_eq!(e.page_size, 128 * 1024);
        assert_eq!(e.ext_align, 128 * 1024);
        assert_eq!((e.state_offset, e.state_length), (0, 128 * 1024));
        assert_eq!((e.dfu_offset, e.dfu_length), (128 * 1024, 512 * 1024));
        assert_eq!(e.config_offset, 640 * 1024);
    }

    /// Мелкие страницы (G0: 2 KiB): bootloader ровно `BOOTLOADER_TARGET`,
    /// `ACTIVE` кратен 4 KiB — стиранию внешней NOR, — а не 2 KiB чипа.
    #[test]
    fn external_layout_on_small_pages_rounds_active_to_the_external_sector() {
        let e =
            compute_external_ota("test", FLASH_BASE, 128 * 1024 + 2048, 2048).expect("помещается");
        assert_eq!(e.bootloader_length, 32 * 1024);
        assert_eq!(e.active_length, 96 * 1024);
        assert_eq!(e.page_size, 4 * 1024);
        assert_eq!(e.ext_align, 64 * 1024);
        assert_eq!(e.dfu_length, 128 * 1024);
        // Журнал — на страницу чипа: (2 + 4*48) слов по 32 байта — 6208
        // байт, один блок.
        assert_eq!(e.state_length, 64 * 1024);
    }

    #[test]
    fn external_layout_reports_a_chip_too_small_for_it() {
        let note =
            compute_external_ota("test", FLASH_BASE, 32 * 1024, 1024).expect_err("не помещается");
        assert!(note.contains("ни страницы"), "{note}");
    }

    #[test]
    fn chip_with_too_few_sectors_reports_why() {
        // H723VE: 512 KiB одним регионом с сектором 128 KiB — 4 сектора, а
        // схеме нужно минимум 5 (BOOTLOADER + STATE + ACTIVE + 2×DFU).
        let layout = compute_memory_layout("test", &uniform_flash(512 * 1024, 128 * 1024, 32))
            .expect("раскладка должна посчитаться");
        let note = layout.ota.expect_err("OTA не должна помещаться");
        assert!(note.contains("512 KiB"), "{note}");
        assert!(note.contains("нужно 3"), "{note}");
        // Но FLASH/RAM всё равно посчитаны — memory.x не остаётся заглушкой.
        assert_eq!(layout.flash_length, 512 * 1024);
        assert_ne!(layout.panic.1, 0, "дамп паники — единственный резерв в RAM");
        assert_eq!(
            layout.panic.0,
            layout.ram_origin + layout.ram_length,
            "PANIC начинается там, где кончается RAM"
        );
    }

    /// Резерв — доля RAM, зажатая между минимумом и максимумом: на 2 KiB это
    /// 128 байт (не половина памяти), на 8 KiB и больше — 512 (сообщение всё
    /// равно обрезается по региону).
    #[test]
    fn panic_region_is_a_clamped_fraction_of_ram() {
        for (ram, expected) in [
            (2 * 1024, 128),
            (4 * 1024, 256),
            (8 * 1024, 512),
            (192 * 1024, 512),
        ] {
            let layout =
                compute_memory_layout("test", &uniform_flash_with_ram(512 * 1024, 2048, 8, ram))
                    .expect("раскладка должна посчитаться");
            assert_eq!(layout.panic.1, expected, "RAM {ram}");
            assert_eq!(layout.ram_length, ram - expected, "RAM {ram}");
        }
    }

    /// Раздел настроек — две последние страницы; OTA считается от того, что
    /// осталось. Не кратный странице flash или отсутствие места под приложение —
    /// `None`.
    #[test]
    fn config_partition_takes_the_last_two_pages() {
        let regions = uniform_flash(512 * 1024, 2048, 8);
        let chain: Vec<&RawRegion> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::Flash)
            .collect();
        let config = compute_config_partition("test", &chain, 512 * 1024, 2048, 8, None)
            .expect("раздел должен поместиться");
        assert_eq!(config.tail.length, 4096);
        assert_eq!(config.flash_length, 512 * 1024 - 4096);
        assert_eq!(config.tail.origin, FLASH_BASE + config.flash_length);

        // Не кратно странице.
        assert!(compute_config_partition("test", &chain, 512 * 1024 + 8, 2048, 8, None).is_none());
        // Две страницы целиком уходят под раздел — приложению ничего.
        assert!(compute_config_partition("test", &chain, 2 * 2048, 2048, 8, None).is_none());
        // Ровно одна страница остаётся приложению — граница включительно.
        assert!(compute_config_partition("test", &chain, 3 * 2048, 2048, 8, None).is_some());
    }

    fn region(name: &str, kind: RegionKind, address: u64, size: u64) -> RawRegion {
        RawRegion {
            name: name.to_string(),
            kind,
            address,
            size,
            flash: None,
        }
    }

    #[test]
    fn extra_regions_skip_covered_and_comment_out_aliases_and_external_buses() {
        let regions = [
            region("SRAM", RegionKind::Ram, RAM_BASE, 64 * 1024),
            region("CCMRAM", RegionKind::Ram, 0x1000_0000, 64 * 1024),
            region("SRAM2", RegionKind::Ram, 0x3000_0000, 16 * 1024),
            region("SRAM2_ICODE", RegionKind::Ram, 0x1000_8000, 16 * 1024),
            region("FMC_BANK1", RegionKind::Ram, 0x6000_0000, 262_144_000),
            region("EMPTY", RegionKind::Ram, 0x4000_0000, 0),
            region("EEPROM", RegionKind::Eeprom, 0x0808_0000, 2048),
        ];
        // SRAM (основная RAM) покрыта хуком, EMPTY нулевого размера пропущен.
        let lines = extra_region_lines(
            "test",
            &regions,
            (FLASH_BASE, FLASH_BASE + 512 * 1024),
            (RAM_BASE, RAM_BASE + 64 * 1024),
            None,
        );
        let joined = lines.join("\n");
        assert_eq!(lines.len(), 5, "{joined}");
        assert!(!joined.contains("EMPTY"), "{joined}");
        assert!(!joined.contains(" SRAM "), "{joined}");
        let find = |name: &str| {
            lines
                .iter()
                .find(|l| l.contains(name))
                .unwrap_or_else(|| panic!("{name} нет в:\n{joined}"))
        };
        // Обычный регион — рабочая строка с правами по виду.
        assert!(find("CCMRAM").contains("(xrw) : ORIGIN = 0x10000000, LENGTH = 64K"));
        assert!(!find("CCMRAM").contains("/*"));
        assert!(find("EEPROM").contains("(r)"));
        // Алиас и внешняя шина закомментированы.
        assert!(find("SRAM2_ICODE").contains("/* "));
        assert!(find("SRAM2_ICODE").contains("второе окно"));
        assert!(find("FMC_BANK1").contains("внешняя шина"));
        // Сортировка по адресу: CCMRAM (0x1000_0000) раньше SRAM2_ICODE (0x1000_8000).
        assert!(joined.find("CCMRAM") < joined.find("SRAM2_ICODE"));
    }

    /// H743: RAM — AXI-SRAM (самая большая область), а не DTCM с `RAM_BASE`;
    /// `DMA_NC` — её хвост (32 KiB), PANIC — под ним. DTCM и SRAM домена D2
    /// остаются своими строками.
    #[test]
    fn h7_ram_is_the_largest_block_with_dma_nc_at_its_tail() {
        let regions = h7_dual_bank(1024 * 1024);
        let layout = compute_memory_layout("h743zi", &regions).expect("раскладка");
        assert_eq!(layout.ram_origin, 0x2400_0000);
        assert_eq!(layout.ram_length + layout.panic.1, 480 * 1024);
        assert_eq!(layout.panic.0 + layout.panic.1, 0x2407_8000);
        assert_eq!(
            layout.dma_nc,
            Some(DmaNc {
                origin: 0x2407_8000,
                length: 32 * 1024
            })
        );
        let joined = layout.extra_regions.join("\n");
        assert!(joined.starts_with("    DMA_NC"), "{joined}");
        assert!(!joined.contains("AXISRAM"), "{joined}");
        for region in ["DTCM", "SRAM1", "SRAM4"] {
            assert!(joined.contains(region), "{region} нет в:\n{joined}");
        }

        // Ядро CM4 того же кристалла — в SRAM домена D2, мимо AXI-SRAM ядра CM7.
        let cm4 = compute_memory_layout("h745zi-cm4", &regions).expect("раскладка");
        assert_eq!(cm4.ram_origin, H7_D2_SRAM);
        assert_eq!(cm4.dma_nc, None);
        assert!(cm4.extra_regions.iter().any(|l| l.contains("AXISRAM")));
    }

    /// Карта памяти H743/H745: два банка по `bank` с сектором 128 KiB, банк 2 —
    /// с `0x0810_0000` (стык при 1 MiB, разрыв при 512 KiB), RAM — как у H7.
    fn h7_dual_bank(bank: u64) -> Vec<RawRegion> {
        let flash = Some(FlashSettings {
            erase_size: 128 * 1024,
            write_size: 32,
            erase_value: 0xFF,
        });
        vec![
            RawRegion {
                name: "BANK_1".to_string(),
                kind: RegionKind::Flash,
                address: FLASH_BASE,
                size: bank,
                flash,
            },
            RawRegion {
                name: "BANK_2".to_string(),
                kind: RegionKind::Flash,
                address: H7_BANK2,
                size: bank,
                flash,
            },
            region("DTCM", RegionKind::Ram, RAM_BASE, 128 * 1024),
            region("AXISRAM", RegionKind::Ram, 0x2400_0000, 512 * 1024),
            region("SRAM1", RegionKind::Ram, H7_D2_SRAM, 128 * 1024),
            region("SRAM4", RegionKind::Ram, H7_SRAM4, 64 * 1024),
        ]
    }

    /// Все разделы внутреннего flash раскладки: `(имя, начало, длина)`.
    fn flash_partitions(layout: &MemoryLayout) -> Vec<(&'static str, u64, u64)> {
        let mut parts = vec![("FLASH", layout.flash_origin, layout.flash_length)];
        let mut ota = |bootloader: &'static str, p: &OtaPartitions| {
            parts.extend([
                (bootloader, layout.flash_origin, p.bootloader_length),
                (
                    "STATE",
                    p.bootloader_state_origin,
                    p.bootloader_state_length,
                ),
                ("ACTIVE", p.active_origin, p.active_length),
                ("DFU", p.dfu_origin, p.dfu_length),
            ]);
            if let Some(s) = p.settings {
                parts.push(("CONFIG", s.origin, s.length));
            }
        };
        if let Ok(p) = &layout.ota {
            ota("BOOTLOADER", p);
        }
        if let Some(config) = &layout.config {
            if let Ok(p) = &config.ota {
                ota("BOOTLOADER+CONFIG", p);
            }
            parts.push(("CONFIG", config.tail.origin, config.tail.length));
            parts.push(("FLASH+CONFIG", layout.flash_origin, config.flash_length));
        }
        if let Ok(e) = &layout.external {
            parts.push(("ACTIVE (ext)", e.active_origin, e.active_length));
        }
        parts
    }

    /// STM32WB: RAM — только SRAM1, SRAM2a/2b за CPU2 и в соседних регионах
    /// закомментированы; все разделы flash кончаются ниже стека CPU2
    /// (`BLE_Stack_full`) — на каждом объёме из таблицы.
    #[test]
    fn wb_layout_leaves_the_cpu2_memory_alone() {
        for (size, cpu2) in WB_CPU2_FLASH {
            let erase_size = if size == 320 * 1024 { 2048 } else { 4096 };
            let mut regions = flash_with_erase_value(size, erase_size, 8, 0xFF);
            regions.retain(|r| r.kind != RegionKind::Ram);
            regions.extend([
                region("SRAM2A_ICODE", RegionKind::Ram, 0x1000_0000, 32 * 1024),
                region("SRAM", RegionKind::Ram, RAM_BASE, 192 * 1024),
                region("SRAM2A", RegionKind::Ram, 0x2003_0000, 32 * 1024),
                region("SRAM2B", RegionKind::Ram, 0x2003_8000, 32 * 1024),
            ]);
            let layout = compute_memory_layout("wb55rg", &regions).expect("раскладка");
            assert_eq!(layout.ram_origin, RAM_BASE);
            assert_eq!(
                layout.panic.0 + layout.panic.1,
                0x2003_0000,
                "{size}: PANIC — в конце SRAM1"
            );
            assert_eq!(layout.flash_origin + layout.flash_length, cpu2);
            assert!(layout.ota.is_ok(), "{size}: OTA не поместилась");
            for (name, origin, length) in flash_partitions(&layout) {
                assert!(
                    origin + length <= cpu2,
                    "{size}: {name} {origin:#x}+{length:#x} заходит за {cpu2:#x}"
                );
            }
            for sram2 in ["SRAM2A ", "SRAM2B ", "SRAM2A_ICODE"] {
                let line = layout
                    .extra_regions
                    .iter()
                    .find(|l| l.contains(sram2))
                    .unwrap_or_else(|| panic!("{sram2} нет в {:?}", layout.extra_regions));
                assert!(line.contains("/* ") && line.contains("CPU2"), "{line}");
            }
        }
        // У WBA ядро одно, и SRAM2 — обычная память.
        let mut wba = flash_with_erase_value(1024 * 1024, 8192, 16, 0xFF);
        wba.push(region(
            "SRAM2",
            RegionKind::Ram,
            RAM_BASE + 64 * 1024,
            64 * 1024,
        ));
        let layout = compute_memory_layout("wba55cg", &wba).expect("раскладка");
        assert_eq!(layout.ram_length + layout.panic.1, 128 * 1024);
        assert_eq!(layout.flash_length, 1024 * 1024);
    }

    /// CM4 двухъядерного H7 — целиком в банке 2, с точки своего старта. При
    /// 2×512 KiB банк 2 цельному `Flash` embassy недоступен: OTA и настройки
    /// отключены, а не разложены туда, куда прошивка не напишет.
    #[test]
    fn h7_cm4_lives_in_bank_2() {
        let xi = compute_memory_layout("h745zi-cm4", &h7_dual_bank(1024 * 1024)).expect("xI");
        assert_eq!((xi.flash_origin, xi.flash_length), (H7_BANK2, 1024 * 1024));
        assert!(
            xi.ota.is_ok() && xi.external.is_ok(),
            "OTA на xI помещается"
        );
        assert!(
            xi.config.as_ref().is_some_and(|c| c.ota.is_ok()),
            "OTA с настройками на xI помещается"
        );
        for (name, origin, _) in flash_partitions(&xi) {
            assert!(origin >= H7_BANK2, "{name} с {origin:#x} — в банке 1");
        }
        let bank1 = xi
            .extra_regions
            .iter()
            .find(|l| l.contains("BANK_1"))
            .expect("банк 1 перечислен");
        assert!(bank1.contains("/* "), "{bank1}");

        let xg = compute_memory_layout("h745zg-cm4", &h7_dual_bank(512 * 1024)).expect("xG");
        assert_eq!((xg.flash_origin, xg.flash_length), (H7_BANK2, 512 * 1024));
        assert!(xg.ota.is_err() && xg.external.is_err() && xg.config.is_none());
    }

    /// CM7 двухъядерного H7 не трогает первый сектор банка 2 — точку старта
    /// CM4: приложение без OTA — в банке 1, `DFU` — за этим сектором, со
    /// смещениями от начала банка (`Bank2Region`).
    #[test]
    fn h7_cm7_keeps_the_cm4_boot_sector_free() {
        for (chip, bank) in [("h745zi-cm7", 1024 * 1024), ("h745zg-cm7", 512 * 1024)] {
            let layout = compute_memory_layout(chip, &h7_dual_bank(bank)).expect("раскладка");
            assert_eq!(
                (layout.flash_origin, layout.flash_length),
                (FLASH_BASE, bank)
            );
            assert_eq!(layout.dfu_bank_origin, Some(H7_BANK2), "{chip}");
            let ota = layout.ota.as_ref().expect("OTA помещается");
            assert_eq!(ota.dfu_origin, CM4_BOOT_SECTOR.end, "{chip}");
            assert!(ota.dfu_origin + ota.dfu_length <= H7_BANK2 + bank, "{chip}");
            for (name, origin, length) in flash_partitions(&layout) {
                assert!(
                    origin + length <= CM4_BOOT_SECTOR.start || origin >= CM4_BOOT_SECTOR.end,
                    "{chip}: {name} {origin:#x}+{length:#x} задевает первый сектор банка 2"
                );
            }
            let bank2 = layout
                .extra_regions
                .iter()
                .find(|l| l.contains("BANK_2"))
                .expect("банк 2 перечислен");
            assert!(bank2.contains("/* "), "{chip}: {bank2}");
        }
    }

    /// Нарушенный инвариант называет чип — иначе по одной панике на 1479 чипов
    /// не понять, чья раскладка сломана.
    #[test]
    #[should_panic(expected = "STM32TEST")]
    fn boot_invariant_panic_names_the_chip() {
        let broken = OtaPartitions {
            bootloader_length: 0,
            bootloader_state_origin: FLASH_BASE,
            bootloader_state_length: 2048,
            bootloader_state_erase_size: 2048,
            swap_page_size: 2048,
            active_origin: FLASH_BASE + 2048,
            active_length: 3000,
            dfu_origin: FLASH_BASE + 2048 + 3000,
            dfu_length: 6000,
            settings: None,
        };
        assert_embassy_boot_invariants("STM32TEST", &broken, FLASH_BASE, 2048, 8);
    }
}
