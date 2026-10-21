//! Проверка шаблона целиком: сгенерировать проект и убедиться, что он
//! собирается — то, что до сих пор делалось руками по инструкции из CLAUDE.md
//! (найти все `{{...}}`/`{% ... %}`, подставить значения, выбрать ветку
//! Liquid-условия, собрать, откатить правки, не забыть проверить `git diff`).
//!
//! Запуск (крейт не член ни одного workspace — только через `--manifest-path`):
//!
//! ```text
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- --quick
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- stm32g071rb
//! ```
//!
//! Ловит ровно тот класс поломок, который не виден ни линтом, ни тестами
//! репозитория шаблона: у самого шаблона `Cargo.lock` пинит ревизии
//! git-зависимостей, поэтому ломающее изменение в `rust-lib` локально не
//! видно. Сгенерированный проект lock-файлов из шаблона не получает и
//! резолвит зависимости заново — так что каждая конфигурация собирается на
//! текущих версиях, как собрался бы свежий проект пользователя.
//!
//! Конфигурации по умолчанию подобраны так, чтобы задеть все ветки
//! генерации: обычный одноядерный, чип с выбором банковой схемы, чип, куда
//! OTA не помещается, тот же обычный чип с OTA, отключённой при генерации,
//! двухъядерный (`init_primary` вместо `init`), чип с 2 KiB RAM на
//! Cortex-M0+, раздел настроек с OTA на равных секторах и без OTA (оба — в
//! хвосте flash), с OTA на неравных (в мелких секторах перед ACTIVE), во
//! внутренней EEPROM и на внешней флеши (слот с заглушкой), подпись образа (с графом и без) и чип, у которого
//! стёртый флеш читается нулями, два протокола поверх UART (Modbus и protobuf)
//! CANopen на FDCAN и bxCAN, Modbus TCP (с master), protobuf UDP, protobuf и
//! Modbus по USB (Modbus без master — по UART и по USB); чипы с клоком RNG через мультиплексор (G0, WL, WBA, L4) и по одному
//! чипу на каждый вариант регистров `banks.rs` (`g0`, `g4`, `l4`, `l5`), а
//! также несколько транспортов разом (`links`: два UART и TCP, два USB с графом
//! и без, TCP и UDP, UART и CAN, UART и UDP без графа) и транспорты без протокола
//! (UDP, USB с графом). Итого пятьдесят два — точный список
//! в `default_cases()`. Свой набор чипов —
//! позиционными аргументами; `--define k=v` (многократно) добавляет определение
//! к каждой конфигурации, например `--ci gitlab --define config=internal stm32f407ve`.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

/// Каждая запись здесь включает свою ветку генерации — см. doc-комментарий
/// выше. Менять только вместе с пониманием, какую ветку теряем.
///
/// Второе поле — суффикс имени каталога: он нужен там, где один и тот же чип
/// проверяется в разных конфигурациях, иначе прогоны затирали бы друг друга.
/// Третье — дополнительные `--define`.
fn default_cases() -> Vec<Case> {
    vec![
        // Одноядерный с OTA и подписью образа (она всегда идёт вместе с OTA:
        // проверка в bootloader'е `screen_update`, ключ через `boot/build.rs`,
        // `salty` со своей `slow-motion` и профилем dev, в `app/src/image.rs`
        // — `FW_VERSION` для `Mode::Signed`), одна карта памяти — самый
        // обычный случай.
        Case::new("stm32f407ve"),
        // Тот же чип, но от OTA отказались при генерации: ветка «один образ на
        // весь flash» на чипе, куда схема прекрасно помещается.
        Case::new("stm32f407ve").variant("no-ota", &["ota=no"]),
        // Несколько карт памяти: в crates-cross/Cargo.toml должна появиться фича
        // "single-bank", без неё build.rs embassy-stm32 паникует.
        Case::new("stm32f429zg"),
        // Банк 2 за разрывом (H743ZG: банк 2 с 0x0810_0000): `boot` и `bsp`
        // разбирают флеш на `Bank1Region`/`Bank2Region`, `DFU` пишется через
        // второй, подпись проверяется над обоими банками (`screen` со своей
        // веткой). Без `config=internal`: с настройками во flash на этом чипе
        // OTA не помещается (`config_ota = false`), `boot` в проект не входит,
        // и ветка `dfu_bank` не собиралась бы вовсе.
        Case::new("stm32h743zg").variant("dfu-bank", &[]),
        // С завода двухбанковый флеш при одобанковой раскладке: `bsp/src/banks.rs`
        // (вариант `g4`) в `bsp`, его зовут и `Board::new`, и `boot`; `unstable-pac` — у `bsp`.
        Case::new("stm32g474re"),
        // OTA не помещается: crates-cross/boot удаляется из проекта, members и xtask
        // должны это пережить.
        //
        // Заодно — настройки на внешней флеши (`config=external`): слот
        // хранилища с заглушкой `bsp::stub::Flash`. Без OTA общий
        // объект `Flash` при этом не нужен вовсе (`shared_flash = false`), и
        // это единственная конфигурация, где bsp собирается без него, а
        // `config::new` — без аргумента. H7 — потому что именно у него раздел
        // во внутреннем флеше дороже всего (2×128 KiB).
        Case::new("stm32h723ve").variant("config-external", &["config=external"]),
        // Двухъядерный: bsp/boot получают init_primary() с SharedData.
        //
        // Ядро именно CM4, а не CM7: у двухъядерных H7 блок сторожа зовётся
        // `IWDG2` у CM4 и `IWDG1` у CM7 (см. таблицу WATCHDOG в
        // chip-data/watchdog.rhai), и `IWDG2` больше не покрыт ничем другим —
        // `IWDG1` берёт на себя одноядерный `stm32h723ve` выше. До появления
        // этой таблицы обе половины давали одинаковый вывод, и выбор ядра
        // здесь ничего не решал.
        Case::new("stm32h745zi-cm4"),
        // Другой конец линейки: 2 KiB RAM и Cortex-M0+ (thumbv6m, без CAS в железе).
        // Резерв под PANIC здесь считается долей RAM, а не фиксированным килобайтом —
        // на таком чипе тот был бы половиной памяти.
        Case::new("stm32l011f4"),
        // Раздел настроек вместе с OTA на равных секторах (2 KiB): в хвосте
        // flash, страница раздела — страница чипа, схема OTA после него всё ещё
        // помещается — в проект попадают разом bootloader, ota.rs и config.rs.
        Case::new("stm32g071rb").variant("config", &["config=internal"]),
        // Он же без OTA: две последние страницы flash (таблица векторов
        // приложения стоит в начале). Ветка «config есть, bootloader и ota.rs
        // удалены» — только здесь.
        Case::new("stm32g071rb").variant("config-no-ota", &["config=internal", "ota=no"]),
        // Раздел настроек вместе с OTA на неравных секторах: у F407 он ложится
        // в сектора 2–3 по 16 KiB перед ACTIVE, `STATE` — в сектор 4 (64 KiB);
        // `CONFIG_ERASE_SIZE` и `STATE_ERASE_SIZE` меньше страницы чипа, оба
        // раздела — над `ErasePage`. В проект попадают разом bootloader, ota.rs и
        // config.rs.
        Case::new("stm32f407ve").variant("config", &["config=internal"]),
        // OTA с `STATE`/`DFU` на внешней флеши (`ota=external`) — на чипе, куда
        // внутренняя схема не помещается вовсе. Разом: слот драйвера в `boot`
        // (блокирующий, с запасным выходом `BootCurrent`) и в `bsp::external`
        // (асинхронный, общий мьютекс), проверка подписи над внешними
        // разделами, настройки на той же
        // микросхеме за `DFU` (`CONFIG_START`/`CONFIG_END` — смещения от её начала), и
        // `shared_flash = false` при OTA.
        Case::new("stm32h723ve").variant("ota-external", &["ota=external", "config=external"]),
        // Внешняя OTA и настройки во внутреннем флеше: на F4 — в секторах 2–3
        // зоны bootloader'а, которые без них пропадали бы. Без графа — `boot`
        // без сторожа, а `embedded-storage` ему нужен только ради геометрии
        // драйвера.
        Case::new("stm32f407ve").variant(
            "ota-external-config",
            &["ota=external", "config=internal", "graph=no"],
        ),
        // Флеш, стирающийся в ноль, с внешней OTA: `flash-erase-zero` у
        // `embassy-boot` выключен — `STATE` на внешней NOR, а она стирается в
        // единицы.
        Case::new("stm32l073rz").variant("ota-external", &["ota=external"]),
        // OTA без графа: узел OTA запускает сам `main` (`ota_task`;
        // `app/src/image.rs` есть, `graph.rs` нет), а bootloader — без сторожа.
        // Чип — Cortex-M0+: проверка подписи (она есть всегда, когда есть OTA)
        // живёт в bootloader'е, и код `salty` под Thumb-1 заметно толще, чем
        // под M4; влезает ли release-bootloader в свою зону на M0+, проверяет эта
        // конфигурация.
        Case::new("stm32g071rb").variant("no-graph", &["graph=no"]),
        // Единственная ветка, где стёртый флеш читается нулями: у L0/L1 хук
        // включает `flash-erase-zero` у embassy-boot, иначе bootloader читает
        // признак «стёрто» наоборот. Взят чип с запасом flash — на 64 KiB
        // раздел ACTIVE выходит меньше минимальной debug-прошивки (см. там же
        // известные ограничения).
        //
        // Заодно — настройки во внутренней EEPROM (`config=internal` на L0/L1):
        // `adapters::eeprom::Eeprom` поверх `Flash::eeprom_*`, единственное место
        // под настройки на флеше, стирающемся в ноль.
        Case::new("stm32l073rz").variant("config", &["config=internal"]),
        // Протоколы поверх UART: узел в графе, заглушка `bsp::serial` и
        // транспорт из `adapters` вокруг неё. Modbus — на USART v4 (G4, есть
        // аппаратный DE), protobuf — на F4 (USART v2, без него): оба
        // интерфейса из таблицы chip-data/interfaces.rhai.
        Case::new("stm32g474re").variant("modbus-uart", &["protocol=modbus", "interface=uart"]),
        Case::new("stm32f407ve").variant("protobuf-uart", &["protocol=protobuf", "interface=uart"]),
        // CANopen на обоих CAN-контроллерах: `can_kind` — FDCAN (G4) и bxCAN
        // (F4) — различает документацию и имя блока в справочнике. Заодно —
        // крейт словаря `crates-host/canopen`, который в остальных
        // конфигурациях удаляется.
        Case::new("stm32g474re").variant("canopen-fdcan", &["protocol=canopen", "interface=can"]),
        Case::new("stm32f407ve").variant("canopen-bxcan", &["protocol=canopen", "interface=can"]),
        // CANopen с разделом настроек: объекты по `save` и адрес по LSS — в
        // хранилище настроек (`domain::canopen`), колбэки стека — `StaticCell`
        // в графе.
        Case::new("stm32g474re").variant(
            "canopen-config",
            &["protocol=canopen", "interface=can", "config=internal"],
        ),
        // То же над заглушкой внешней флеши (`config=external`): у её ошибки
        // свой `defmt::Format`.
        Case::new("stm32f407ve").variant(
            "canopen-config-external",
            &["protocol=canopen", "interface=can", "config=external"],
        ),
        // Протоколы по сети: стек `embassy-net` над заглушкой устройства, узел
        // NET в графе и сокет из `adapters::net`. Modbus TCP — на H7 (Ethernet
        // v2), protobuf UDP — на F4 (Ethernet v1). Modbus TCP — заодно с master
        // (`modbus_master=yes`): единственная конфигурация, где клиентская
        // половина (`ports::ModbusClient`, `domain::modbus::master`, клиентские
        // impl'ы транспортов) собирается в сгенерированном проекте; остальные
        // Modbus (`modbus-uart`, `modbus-usb`) — без неё, по умолчанию.
        Case::new("stm32h743zg").variant(
            "modbus-tcp-master",
            &["protocol=modbus", "interface=tcp", "modbus_master=yes"],
        ),
        Case::new("stm32f407ve").variant("protobuf-udp", &["protocol=protobuf", "interface=udp"]),
        // Транспорт без протокола (свой протокол пишет проект): заглушка интерфейса
        // и его фоновый узел есть, узла протокола — нет. Граф и его отсутствие, по
        // одному интерфейсу на вид: поток (UART), шина (CAN), сеть, USB.
        Case::new("stm32g474re").variant("uart-only", &["interface=uart"]),
        Case::new("stm32g474re").variant("can-only-no-graph", &["interface=can", "graph=no"]),
        Case::new("stm32h743zg").variant("tcp-only", &["interface=tcp"]),
        Case::new("stm32g474re").variant("usb-only-no-graph", &["interface=usb", "graph=no"]),
        // Остальные транспорты по сети: Modbus UDP (MBAP в датаграмме, без потока
        // и CRC) и protobuf по TCP (COBS поверх сокета) — у каждого свой набор
        // файлов в `adapters`, остальные при генерации вырезаются.
        Case::new("stm32h743zg").variant("modbus-udp", &["protocol=modbus", "interface=udp"]),
        Case::new("stm32h743zg").variant("protobuf-tcp", &["protocol=protobuf", "interface=tcp"]),
        // protobuf по USB CDC: устройство `embassy-usb`, узел USB и поток CDC
        // под COBS. G4 — блок USB device (у F4/H7 — OTG, драйвер тот же).
        Case::new("stm32g474re").variant("protobuf-usb", &["protocol=protobuf", "interface=usb"]),
        // Modbus RTU по USB CDC — на F4, у которого USB — блок OTG: второй
        // драйвер в рецепте и `Rtu` поверх потока CDC вместо UART.
        Case::new("stm32f407ve").variant("modbus-usb", &["protocol=modbus", "interface=usb"]),
        // Протоколы без графа: узел спавнит `tasks::spawn_all` (`app/src/tasks/`),
        // а `NET`/`USB` — рядом с ним. По одной конфигурации на вид
        // интерфейса: UART, TCP, UDP, USB CDC и CAN (с настройками — слот
        // хранилища узла CANopen).
        Case::new("stm32g474re").variant(
            "modbus-uart-no-graph",
            &["protocol=modbus", "interface=uart", "graph=no"],
        ),
        Case::new("stm32h743zg").variant(
            "modbus-tcp-no-graph",
            &["protocol=modbus", "interface=tcp", "graph=no"],
        ),
        Case::new("stm32f407ve").variant(
            "protobuf-udp-no-graph",
            &["protocol=protobuf", "interface=udp", "graph=no"],
        ),
        Case::new("stm32g474re").variant(
            "protobuf-usb-no-graph",
            &["protocol=protobuf", "interface=usb", "graph=no"],
        ),
        Case::new("stm32g474re").variant(
            "canopen-no-graph",
            &[
                "protocol=canopen",
                "interface=can",
                "config=internal",
                "graph=no",
            ],
        ),
        // Несколько транспортов (`links`): у каждого своя группа ресурсов,
        // подмодуль драйвера в `bsp` и транспорта в `app`, поле `Board` (или
        // драйвер в `Drivers`) и узел графа (без графа — модуль задачи). Два UART
        // одного вида рядом с сетью (H7: три узла Modbus и один `NET`), два USB
        // (два устройства с разными серийными номерами; без графа — и по
        // сигналу готовности OTA на каждое), TCP и UDP на одном стеке
        // (`SOCKETS` = 1 + 2), поток с протоколом рядом с шиной без протокола,
        // UART с UDP без графа. Транспорты без протокола: UDP (сокет и
        // `PacketMetadata` ждут ваш узел) и USB с графом (поток CDC в
        // `Parts` никто не забирает).
        Case::new("stm32h743zg").variant(
            "modbus-uart-uart-tcp",
            &["links=uart:modbus,uart:modbus,tcp:modbus"],
        ),
        Case::new("stm32f407ve").variant("protobuf-usb-usb", &["links=usb:protobuf,usb:protobuf"]),
        Case::new("stm32f407ve").variant(
            "protobuf-usb-usb-no-graph",
            &["links=usb:protobuf,usb:protobuf", "graph=no"],
        ),
        Case::new("stm32h743zg").variant("udp-only", &["links=udp"]),
        Case::new("stm32g474re").variant("usb-only", &["interface=usb"]),
        Case::new("stm32f407ve").variant("modbus-tcp-udp", &["links=tcp:modbus,udp:modbus"]),
        Case::new("stm32f407ve").variant("modbus-uart-can", &["links=uart:modbus,can"]),
        Case::new("stm32f407ve").variant(
            "protobuf-uart-udp-no-graph",
            &["links=uart:protobuf,udp:protobuf", "graph=no"],
        ),
        // Клок RNG, которого умолчания HAL не дают, — мультиплексор на своём
        // источнике (`config()` в `bsp::board`): `RNGSEL` у G0 (SYS), WL
        // (MSI) и WBA (HSI), `CLK48SEL` у L476 без HSI48 (MSI). PLL48 и HSI48
        // проверяют F4, L0 и G4/H7 выше.
        Case::new("stm32g081rb"),
        Case::new("stm32wle5jc"),
        Case::new("stm32wba52cg"),
        Case::new("stm32l476rg"),
        // Остальные варианты `bsp/src/banks.rs` (`g4` — у g474re выше): регистры
        // FLASH у G0B/G0C (`g0`), L4+ (`l4`) и L5 (`l5`) различаются.
        Case::new("stm32g0b1rc"),
        Case::new("stm32l4r5zi"),
        Case::new("stm32l552ze"),
    ]
}

/// Одна проверяемая конфигурация: чип плюс, если нужно, дополнительные
/// `--define` и суффикс, отличающий её каталог от других прогонов того же чипа.
struct Case {
    chip: String,
    suffix: Option<String>,
    defines: Vec<String>,
}

impl Case {
    fn new(chip: &str) -> Self {
        Self {
            chip: chip.to_owned(),
            suffix: None,
            defines: Vec::new(),
        }
    }

    fn variant(self, suffix: &str, defines: &[&str]) -> Self {
        Self {
            suffix: Some(suffix.to_owned()),
            defines: defines.iter().map(|d| (*d).to_owned()).collect(),
            ..self
        }
    }

    /// Имя проекта, оно же имя каталога.
    fn name(&self) -> String {
        match &self.suffix {
            // Дефис в чип-фиче двухъядерного чипа (`stm32h745zi-cm7`)
            // переносим как есть: cargo-generate санитизирует имя в
            // kebab-case и молча переименовывает каталог, так что `_` привёл
            // бы к поиску несуществующего пути.
            Some(suffix) => format!("tc-{}-{suffix}", self.chip),
            None => format!("tc-{}", self.chip),
        }
    }

    fn label(&self) -> String {
        match &self.suffix {
            Some(suffix) => format!("{} ({suffix})", self.chip),
            None => self.chip.clone(),
        }
    }
}

/// Файлы, где сырые `{{...}}` остаются намеренно и после генерации.
/// `CLAUDE.md` документирует плейсхолдеры как текст и потому исключён из
/// Liquid-подстановки (`exclude` в cargo-generate.toml).
const RAW_PLACEHOLDERS_ALLOWED: &[&str] = &["CLAUDE.md"];

/// Каталоги, которые не обходим при поиске остаточных плейсхолдеров.
const SKIP_DIRS: &[&str] = &[".git", "target"];

/// Что не нужно копии шаблона, из которой идёт генерация: каталоги сборки (на
/// любом уровне — их два, в корне и в `crates-cross/`), история и сам этот
/// инструмент, у которого свой `target/` рядом с исходниками.
const SKIP_IN_TEMPLATE: &[&str] = &[".git", "target", "chip-data-gen"];

struct Options {
    cases: Vec<Case>,
    ci: String,
    /// Общие `--define k=v` для каждой конфигурации (после её собственных).
    defines: Vec<String>,
    /// Только сгенерировать и проверить рендер, без сборки — быстрая проверка
    /// хуков и Liquid-условий (секунды вместо минут на чип).
    quick: bool,
    /// Не удалять сгенерированные проекты после успеха.
    keep: bool,
}

fn main() -> anyhow::Result<()> {
    let options = parse_args()?;
    let repo_root = repo_root();

    // Вне репозитория, и это важнее удобства: `cargo generate --path` копирует
    // каталог шаблона ЦЕЛИКОМ во временную папку, прежде чем прочитать
    // cargo-generate.toml, так что `ignore = ["target", ...]` от копирования
    // не спасает — он решает лишь, что попадёт в готовый проект. Кеш сборки
    // здесь общий на все конфигурации (второй прогон не пересобирает то, что
    // от чипа не зависит) и за десяток прогонов дорастает до гигабайтов;
    // внутри `target/` он превращал каждую генерацию из локального пути в
    // копирование этих гигабайтов. Однажды это кончилось «Недостаточно места
    // на диске (os error 112)» на ровном месте.
    let work_dir = env::temp_dir().join("rust-embedded-template-check");
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("создать рабочий каталог {}", work_dir.display()))?;
    let cargo_target_dir = work_dir.join("cargo-target");

    // Генерируем не из самого репозитория, а из его облегчённой копии — по той
    // же причине, что и work_dir выше: `--path` копирует каталог целиком, и
    // `target/` (гигабайты мелких файлов) уходит в копию на каждую
    // конфигурацию. Замерено: генерация из репозитория с 3 ГБ в `target/` —
    // 169 секунд, из копии без него — секунда. Девять конфигураций превращали
    // это в семнадцать минут ожидания на ровном месте.
    let template = work_dir.join("template");
    copy_template(&repo_root, &template)
        .with_context(|| format!("скопировать шаблон в {}", template.display()))?;

    let labels: Vec<String> = options.cases.iter().map(Case::label).collect();
    println!(
        "Проверяем шаблон на {} конфигурациях (ci={}{}): {}",
        options.cases.len(),
        options.ci,
        if options.quick { ", --quick" } else { "" },
        labels.join(", "),
    );

    let mut static_ram = Vec::new();
    for case in &options.cases {
        if let Some(ram) = check_one(&template, &work_dir, &cargo_target_dir, case, &options)? {
            static_ram.push((case.label(), ram));
        }
    }

    // Факт статической RAM по случаям — из строки `ram:` вывода `build`.
    if !static_ram.is_empty() {
        println!("\nСтатическая RAM (.data + .bss + .uninit), байт:");
        let width = static_ram
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        for (label, ram) in &static_ram {
            println!("  {label:<width$}  {ram:>7}");
        }
    }

    println!(
        "\nГотово: {} конфигураций прошли {}.",
        options.cases.len(),
        if options.quick {
            "генерацию"
        } else {
            "генерацию, lint и сборку"
        },
    );
    Ok(())
}

/// `template` — облегчённая копия, из которой генерируем.
fn check_one(
    template: &Path,
    work_dir: &Path,
    cargo_target_dir: &Path,
    case: &Case,
    options: &Options,
) -> anyhow::Result<Option<u64>> {
    // Имя проекта = имя каталога, в который cargo-generate его положит.
    let name = case.name();
    let project = work_dir.join(&name);
    if project.exists() {
        fs::remove_dir_all(&project)
            .with_context(|| format!("очистить прошлый прогон {}", project.display()))?;
    }

    println!("\n=== {} ===", case.label());
    let mut command = Command::new("cargo");
    command
        .arg("generate")
        .arg("--path")
        .arg(template)
        .arg("--name")
        .arg(&name)
        .arg("--define")
        .arg(format!("chip_feature={}", case.chip))
        .arg("--define")
        .arg(format!("ci={}", options.ci));
    for define in case.defines.iter().chain(&options.defines) {
        command.arg("--define").arg(define);
    }
    run(
        command.arg("--silent").arg("--destination").arg(work_dir),
        work_dir,
        None,
    )
    .with_context(|| format!("генерация проекта под {}", case.label()))?;

    check_no_raw_placeholders(&project)?;
    check_ci_files(&project, &options.ci)?;

    let mut static_ram = None;
    if !options.quick {
        // Ровно те команды, которые README обещает пользователю шаблона.
        //
        // `xtask pins` — единственный способ вообще проверить `chip-info`:
        // его манифест до генерации не резолвится (Liquid в чип-фиче), так что
        // ни `cargo xtask lint`, ни CI самого шаблона его не трогают. Оба
        // вызова не случайны: без аргумента идёт перечисление блоков, с `RCC` —
        // ветка с таблицей выводов, DMA и тактированием, а `RCC` есть на любом
        // STM32 (в отличие от, например, `SPI4`).
        //
        // fmt/clippy для chip-info — здесь же и по той же причине; в
        // сгенерированном проекте манифест уже подставлен, и обе команды
        // работают. `cargo xtask lint` их намеренно не зовёт: тогда каждый
        // пользовательский CI тянул бы stm32-metapac ради инструмента, который
        // на сборку прошивки никак не влияет.
        let commands: [&[&str]; 12] = [
            &["xtask", "lint"],
            &["xtask", "test", "host"],
            &["xtask", "lint", "cross"],
            &["xtask", "build"],
            // Лицензии, источники, уязвимости: и хостовый workspace, и
            // прошивка (у неё свой `Cargo.lock`, он появляется после `build`).
            &["deny", "check"],
            &[
                "deny",
                "--manifest-path",
                "crates-cross/Cargo.toml",
                "check",
            ],
            &["xtask", "pins"],
            &["xtask", "pins", "RCC"],
            // Заготовка — на I2C1, и это не произвольный выбор: он есть на
            // всех чипах набора (у stm32l011f4, например, USART1 нет вовсе —
            // только USART2 и LPUART1), а обработчиков прерывания у него два
            // и они разные, то есть проверяется самая ошибкоопасная ветка
            // таблицы. `--check` на нетронутом `bsp` обязан проходить:
            // шаблон не занимает ни одного вывода.
            &["xtask", "pins", "I2C1", "--snippet"],
            &["xtask", "pins", "--check"],
            &[
                "fmt",
                "--manifest-path",
                "crates-host/chip-info/Cargo.toml",
                "--check",
            ],
            &[
                "clippy",
                "--manifest-path",
                "crates-host/chip-info/Cargo.toml",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ];
        check_snippets_compile(&project, cargo_target_dir)?;

        for args in commands {
            let context = || {
                format!(
                    "`cargo {}` в сгенерированном проекте (оставлен для разбора: {})",
                    args.join(" "),
                    project.display(),
                )
            };
            let command = &mut Command::new("cargo");
            command.args(args);
            if args != ["xtask", "build"] {
                run(command, &project, Some(cargo_target_dir)).with_context(context)?;
                continue;
            }
            // Вывод `build` перехватывается ради строки `ram:` и печатается
            // как есть, чтобы отчёт сборки в логе остался.
            let output =
                capture(command, &project, Some(cargo_target_dir)).with_context(context)?;
            print!("{output}");
            let ram = parse_static_ram(&output).with_context(|| {
                format!(
                    "в выводе `cargo xtask build` нет строки `ram: <N> байт статической RAM …` \
                     (формат — ram_line в crates-host/xtask/src/size.rs) для {}",
                    case.label(),
                )
            })?;
            static_ram = Some(ram);
        }
    }

    if options.keep {
        println!("проект оставлен: {}", project.display());
    } else {
        fs::remove_dir_all(&project).with_context(|| format!("удалить {}", project.display()))?;
    }
    Ok(static_ram)
}

/// Ищет `{{ }}`/`{% %}`, пережившие генерацию. Мимо линта и сборки такое
/// проходит незамеченным везде, кроме `.rs`/`.toml`: `.json`, `.yml` и
/// `memory.x` никто не компилирует, и сломанный `launch.json` обнаружился бы
/// только у пользователя.
fn check_no_raw_placeholders(project: &Path) -> anyhow::Result<()> {
    let mut found = Vec::new();
    visit_files(project, &mut |path| {
        let relative = path.strip_prefix(project).unwrap_or(path);
        let relative_str = relative.to_string_lossy().replace('\\', "/");
        if RAW_PLACEHOLDERS_ALLOWED.contains(&relative_str.as_str()) {
            return Ok(());
        }
        let text = match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(err) => bail!("прочитать {}: {err}", path.display()),
        };
        for (number, line) in text.lines().enumerate() {
            let line_without_actions = without_actions_expressions(line);
            if line_without_actions.contains("{{") || line_without_actions.contains("{%") {
                found.push(format!("{relative_str}:{}: {}", number + 1, line.trim()));
            }
        }
        Ok(())
    })?;

    if !found.is_empty() {
        bail!(
            "в сгенерированном проекте остались неподставленные плейсхолдеры:\n  {}",
            found.join("\n  "),
        );
    }
    println!("плейсхолдеры: не осталось ни одного");
    Ok(())
}

/// Что должно быть в сгенерированном проекте при каждом `ci` — и чего там быть
/// не должно.
///
/// Проверка не про подстановку, а про состав: `[conditional.'ci != "gitlab"']`
/// в `cargo-generate.toml` убирает `.gitlab-ci.yml` и `renovate.json`, а
/// `'ci != "github"'` — весь `.github`. Пока ветки `gitlab` и `none` не
/// генерировались ни разу (дефолт `--ci github`, а CI звал только его), эти
/// два правила не проверял никто: опечатка в условии убрала бы чужой CI из
/// проекта или, наоборот, оставила бы в нём `.gitlab-ci.yml` с Liquid внутри —
/// а Liquid в убранном файле просто не отрендерился бы, и `check_no_raw_placeholders`
/// такой файл не видит: его в проекте нет.
///
/// Отдельная строка про `template.yml`: он исключён поимённо, и это единственный
/// файл, который в проекте не должен появляться никогда, — CI шаблона в
/// сгенерированном проекте запускал бы `template-check` без `rust-lib`.
fn check_ci_files(project: &Path, ci: &str) -> anyhow::Result<()> {
    // (должен быть, не должен быть) относительно корня проекта.
    let (present, absent): (&[&str], &[&str]) = match ci {
        "github" => (
            &[".github/workflows/ci.yml"],
            &[
                ".github/workflows/template.yml",
                ".gitlab-ci.yml",
                "renovate.json",
            ],
        ),
        "gitlab" => (
            &[".gitlab-ci.yml", "renovate.json"],
            &[".github", ".github/workflows/template.yml"],
        ),
        "none" => (&[], &[".github", ".gitlab-ci.yml", "renovate.json"]),
        other => bail!("--ci {other}: ожидались github|gitlab|none"),
    };

    for relative in present {
        if !project.join(relative).exists() {
            bail!("при ci={ci} в проекте нет {relative}");
        }
    }
    for relative in absent {
        if project.join(relative).exists() {
            bail!("при ci={ci} в проекте остался лишний {relative}");
        }
    }
    println!("файлы CI при ci={ci}: как ожидалось");
    Ok(())
}

/// Вырезает выражения GitHub Actions (`${{ secrets.X }}`) — в сгенерированном
/// проекте это рабочий синтаксис, а не переживший генерацию плейсхолдер.
///
/// В шаблоне они обёрнуты в `{% raw %}`, иначе cargo-generate спотыкается о
/// неизвестную переменную и пропускает подстановку ВО ВСЁМ файле (проверено
/// эмпирически: рядом стоявший `{{target}}` тоже оставался сырым). Поэтому
/// послабление ничего не ослабляет: пропусти генерация такой файл целиком —
/// настоящие плейсхолдеры в нём никуда не денутся и будут найдены этой же
/// проверкой.
///
/// Незакрытое `${{` считается вырезанным до конца строки: выражения Actions
/// однострочные, а ложное срабатывание здесь дороже пропуска.
fn without_actions_expressions(line: &str) -> String {
    let mut rest = line;
    let mut out = String::new();
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "${{".len()..];
        match after.find("}}") {
            Some(end) => rest = &after[end + "}}".len()..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

fn visit_files(
    dir: &Path,
    visit: &mut impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("прочитать {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            visit_files(&path, visit)?;
        } else if file_type.is_file() {
            visit(&path)?;
        }
    }
    Ok(())
}

/// Статическая RAM образа из вывода `cargo xtask build` — строка
/// `ram: <N> байт статической RAM из …`, которую печатает
/// `crates-host/xtask/src/size.rs` (`ram_line`). Формат — общий контракт.
fn parse_static_ram(build_output: &str) -> Option<u64> {
    build_output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("ram: ")?
            .split_once(" байт статической RAM")?
            .0
            .parse()
            .ok()
    })
}

/// То же, что [`run`], но отдаёт stdout: нужно там, где проверяется не код
/// возврата, а напечатанное.
fn capture(
    command: &mut Command,
    current_dir: &Path,
    cargo_target_dir: Option<&Path>,
) -> anyhow::Result<String> {
    command.current_dir(current_dir);
    if let Some(target_dir) = cargo_target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let output = command
        .output()
        .with_context(|| format!("запустить {:?}", command.get_program()))?;
    if !output.status.success() {
        bail!(
            "команда завершилась с {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Заготовки из `cargo xtask pins <БЛОК> --snippet` для ВСЕХ блоков чипа —
/// собираются в один временный модуль `bsp` и компилируются.
///
/// Зачем это здесь. Заготовку человек вставляет в свой проект как есть, то
/// есть она обязана компилироваться, — а проверялось до сих пор лишь то, что
/// команда не упала. Цена одного этого пробела: пять дефектов разом, и
/// каждый ронял вставленный код у пользователя. Дублирующиеся ключи там, где
/// два сигнала делят вектор (`error[E0428]` на 217 чипах); имя структуры
/// `Usb_otg_fsResources`, которое не проходит `non_camel_case_types` под
/// `-D warnings`; несуществующий `adc::InterruptHandler` на большинстве
/// версий ADC; пустая `bind_interrupts!` для классического USB; недостающий
/// импорт `Peri`. Ни один из них компилятор здесь не видел, потому что
/// напечатанное никуда не компилировалось.
///
/// Проверяются все блоки с выводами (у STM32F407 их 38), а не выборка: обе
/// половины заготовки зависят от блока, и «на I2C1 сходится» ничего не
/// говорит про USB или ADC.
///
/// Каждая заготовка — в своём модуле: `bind_interrupts!` объявляет `struct
/// Irqs`, и без модулей второй же блок дал бы конфликт имён. Линковки нет,
/// поэтому одинаковые `#[unsafe(no_mangle)]`-обработчики у блоков, делящих
/// вектор, друг другу не мешают: `clippy` только проверяет.
fn check_snippets_compile(project: &Path, cargo_target_dir: &Path) -> anyhow::Result<()> {
    let listing = capture(
        Command::new("cargo").args(["xtask", "pins"]),
        project,
        Some(cargo_target_dir),
    )?;
    let blocks = blocks_with_pins(&listing);
    anyhow::ensure!(
        !blocks.is_empty(),
        "`cargo xtask pins` не перечислил ни одного блока с выводами — разбор списка разошёлся с \
         форматом вывода chip-info",
    );

    // Половина `assign_resources!` берётся ровно у одного блока, и это не
    // экономия: макрос объявляет `split_resources` через `#[macro_export]`, то
    // есть кладёт его в корень крейта — двух вызовов в одном крейте не бывает
    // (`error[E0428]: the name split_resources is defined multiple times`).
    // По той же причине она встаёт не рядом с `bind_interrupts!`, а на место
    // `resources.rs` — туда, куда группу переносит человек.
    // Блок выбирается с подчёркиванием в имени, если такой есть: именно на нём
    // ловится имя структуры ресурсов (`USB_OTG_FS` давал
    // `Usb_otg_fsResources`, который не проходит `non_camel_case_types`).
    let with_resources = blocks
        .iter()
        .find(|block| block.contains('_'))
        .unwrap_or(&blocks[0])
        .clone();

    // В проекте с выбранным интерфейсом `resources.rs` уже не пуст: в нём
    // группы блока интерфейса и RNG, на которые опирается `Board::new`, и
    // подменять файл заготовкой нельзя. Половина `assign_resources!` проверяется
    // тогда в проектах без интерфейса (те же чипы, `resources.rs` — заглушка),
    // а здесь — только `bind_interrupts!`.
    let bsp = project.join("crates-cross").join("bsp").join("src");
    let resources_path = bsp.join("resources.rs");
    let original_resources = fs::read_to_string(&resources_path)?;
    let placeholder_only = original_resources.contains("BoardResources {}");

    let mut resources = String::new();
    let mut module = String::from(
        "//! Временный модуль: сюда `template-check` складывает заготовки из\n\
         //! `cargo xtask pins <БЛОК> --snippet` и компилирует их. В проекте\n\
         //! пользователя этого файла нет — он создаётся и удаляется проверкой.\n\
         #![allow(dead_code, reason = \"заготовки никто не вызывает\")]\n\n",
    );
    for block in &blocks {
        let snippet = capture(
            Command::new("cargo").args(["xtask", "pins", block, "--snippet"]),
            project,
            Some(cargo_target_dir),
        )?;
        let bind = part_of(&snippet, "bind_interrupts!");
        if !bind.is_empty() {
            // Импортов модулю не добавляется ни одного: заготовка печатает
            // свои, и в этом половина смысла проверки — вставленная как есть,
            // она обязана собираться сама по себе.
            module.push_str(&format!(
                "mod irqs_{} {{\n{}\n}}\n\n",
                block.to_lowercase(),
                indent(&bind),
            ));
        }
        if placeholder_only && *block == with_resources {
            resources = part_of(&snippet, "assign_resources!");
            anyhow::ensure!(
                !resources.is_empty(),
                "в заготовке для {block} нет половины assign_resources! — разбор разошёлся с \
                 форматом вывода chip-info",
            );
        }
    }

    let module_path = bsp.join("snippet_check.rs");
    let lib_path = bsp.join("lib.rs");
    let lib = fs::read_to_string(&lib_path)?;
    if placeholder_only {
        fs::write(
            &resources_path,
            format!(
                "{resources}
"
            ),
        )?;
    }
    fs::write(&module_path, &module)?;
    fs::write(&lib_path, format!("{lib}\nmod snippet_check;\n"))?;

    let checked = run(
        Command::new("cargo").args(["clippy", "-p", "bsp", "--", "-D", "warnings"]),
        &project.join("crates-cross"),
        Some(cargo_target_dir),
    );

    // Проект возвращается в исходное состояние в любом случае: следом за этой
    // проверкой идут другие, и им нужен нетронутый `bsp`.
    //
    // Результат восстановления НЕ пробрасывается здесь же: сорвись оно (файл
    // занят, права), сообщение об этом вытеснило бы настоящую причину —
    // «заготовка не компилируется», — ради которой прогон и затевался.
    // Сначала главное, потом уборка.
    // Каждый файл — независимо: сорвись `lib.rs`, `resources.rs` всё равно
    // должен вернуться, иначе следующие проверки идут на чужой распиновке.
    let restored = [
        fs::write(&lib_path, lib),
        fs::write(&resources_path, original_resources),
        fs::remove_file(&module_path),
    ]
    .into_iter()
    .collect::<std::io::Result<()>>();

    checked.context(
        "заготовка из `cargo xtask pins --snippet` не компилируется — её вставляют в проект как \
         есть, значит чинить надо таблицу HANDLERS или печать в crates-host/chip-info",
    )?;
    restored.with_context(|| {
        format!(
            "заготовки компилируются, но временный модуль не убран — уберите вручную: {}",
            module_path.display(),
        )
    })?;
    println!("заготовки: {} блоков компилируются", blocks.len());
    Ok(())
}

/// Имена блоков из шапки `cargo xtask pins` без аргументов.
///
/// Формат её вывода — «блоки с выводами (N):» и дальше строки с именами через
/// пробел, до пустой строки.
fn blocks_with_pins(listing: &str) -> Vec<String> {
    listing
        .lines()
        .skip_while(|line| !line.starts_with("блоки с выводами"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect()
}

/// Одна половина напечатанной заготовки: от строки, начинающейся с `head`, и
/// до конца её блока — то есть до строки, где закрывается верхний уровень
/// скобок.
///
/// Пустая строка означает, что половины нет вовсе: `bind_interrupts!` не
/// печатается блокам, у которых в embassy нет обработчика.
///
/// Комментарии в заготовке адресованы человеку («вставьте это в main.rs») и в
/// модуль не нужны.
fn part_of(snippet: &str, head: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    // Импорты половины печатаются перед её макросом, и без них она не
    // компилируется, — значит забирать надо вместе. Сбрасываются они на
    // комментарии-заголовке: `use` принадлежит той половине, за которой стоит.
    let mut imports: Vec<&str> = Vec::new();
    let mut depth = 0usize;
    let mut started = false;
    for line in snippet.lines() {
        if !started {
            if line.starts_with("// crates-cross/") {
                imports.clear();
            } else if line.starts_with("use ") {
                imports.push(line);
            }
            if !line.starts_with(head) {
                continue;
            }
            started = true;
            lines.append(&mut imports);
            lines.push("");
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        lines.push(line);
        if depth == 0 {
            break;
        }
    }
    lines.join("\n")
}

fn indent(code: &str) -> String {
    code.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(
    command: &mut Command,
    current_dir: &Path,
    cargo_target_dir: Option<&Path>,
) -> anyhow::Result<()> {
    command.current_dir(current_dir);
    if let Some(target_dir) = cargo_target_dir {
        // Общий каталог сборки на все чипы: то, что от чипа не зависит
        // (domain и весь host-граф), собирается один раз на весь прогон.
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let status = command
        .status()
        .with_context(|| format!("запустить {:?}", command.get_program()))?;
    if !status.success() {
        bail!("команда завершилась с {status}");
    }
    Ok(())
}

fn parse_args() -> anyhow::Result<Options> {
    let mut cases: Vec<Case> = Vec::new();
    let mut ci = "github".to_owned();
    let mut defines: Vec<String> = Vec::new();
    let mut quick = false;
    let mut keep = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => quick = true,
            "--keep" => keep = true,
            "--ci" => {
                ci = args
                    .next()
                    .context("--ci требует значения (github|gitlab|none)")?;
            }
            "--define" => {
                defines.push(
                    args.next()
                        .context("--define требует значения (ключ=значение)")?,
                );
            }
            "-h" | "--help" => {
                println!(
                    "USAGE: template-check [--quick] [--keep] [--ci github|gitlab|none] \
                     [--define k=v ...] [chip_feature ...]\n\n\
                     Без чипов проверяются: {}",
                    default_cases()
                        .iter()
                        .map(Case::label)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                std::process::exit(0);
            }
            other if other.starts_with('-') => bail!("неизвестный флаг: {other}"),
            other => cases.push(Case::new(other)),
        }
    }

    if cases.is_empty() {
        cases = default_cases();
    }
    Ok(Options {
        cases,
        ci,
        defines,
        quick,
        keep,
    })
}

fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir
}

/// Копирует шаблон в отдельный каталог, пропуская то, что генерации не нужно:
/// `target/` (гигабайты артефактов), `.git/` и `chip-data-gen/` — сам этот
/// инструмент, у которого свой `target/` рядом с исходниками.
///
/// Прошлая копия сносится целиком: правки шаблона должны доезжать до проверки,
/// а не оставаться в устаревшем слепке.
fn copy_template(repo_root: &Path, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)
            .with_context(|| format!("очистить прошлую копию {}", dest.display()))?;
    }
    fs::create_dir_all(dest)?;

    for entry in fs::read_dir(repo_root)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP_IN_TEMPLATE.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("скопировать {} в {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        // Пропуск нужен на каждом уровне, а не только в корне: `crates-cross/target`
        // — такой же каталог сборки, и без этой проверки копия весила 2.2 ГБ
        // вместо десятка мегабайт, а генерация из неё занимала полминуты.
        if SKIP_IN_TEMPLATE.contains(&entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&source, &destination)?;
        } else {
            fs::copy(&source, &destination).with_context(|| {
                format!(
                    "скопировать {} в {}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_static_ram, without_actions_expressions};

    /// Строку печатает `crates-host/xtask/src/size.rs` (`ram_line`).
    #[test]
    fn parses_the_static_ram_line_of_a_build() {
        let output = "app: 41.2 KiB из 128.0 KiB ACTIVE (32.2%)\n\
                      ram: 3124 байт статической RAM из 131072 байт RAM (2.4%)\n\
                      образ для OTA: app.bin\n";

        assert_eq!(parse_static_ram(output), Some(3124));
        assert_eq!(parse_static_ram("app: 41.2 KiB\n"), None);
    }

    /// Главное свойство: послабление ради Actions не должно прятать настоящий
    /// плейсхолдер, стоящий в той же строке.
    #[test]
    fn keeps_a_liquid_placeholder_next_to_an_actions_expression() {
        let line = "        run: gh release create {{project-name}} --token ${{ secrets.TOKEN }}";

        assert!(without_actions_expressions(line).contains("{{"));
    }

    #[test]
    fn strips_an_actions_expression() {
        let line = "          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}";

        let cleaned = without_actions_expressions(line);
        assert!(
            !cleaned.contains("{{"),
            "осталось выражение Actions: {cleaned}"
        );
    }

    /// Несколько выражений в строке — обычное дело для `if:`.
    #[test]
    fn strips_every_actions_expression_in_a_line() {
        let line = "    if: ${{ github.event_name == 'push' }} && ${{ success() }}";

        assert!(!without_actions_expressions(line).contains("{{"));
    }

    /// Незакрытое выражение не должно уводить функцию в бесконечный цикл или
    /// панику по границе среза — только этого и проверяем.
    #[test]
    fn survives_an_unclosed_actions_expression() {
        assert!(!without_actions_expressions("value: ${{ secrets.X").contains("{{"));
    }
}
