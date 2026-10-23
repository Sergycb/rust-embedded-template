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
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- --smoke
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- --only modbus,no-graph
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- stm32g071rb
//! ```
//!
//! Уровни: `--quick` по всему набору — на каждую правку; `--only` по
//! затронутой ветке и `--smoke` — по ходу работы; полный прогон — перед
//! вливанием в master. Конфигурации идут параллельно (`--jobs`, по умолчанию
//! [`DEFAULT_JOBS`]), вывод каждой — в свой лог, в консоли — строка на итог.
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
//! внутренней EEPROM и на внешней флеши (драйвер W25Q над шиной-заглушкой — SPI, QUADSPI,
//! OCTOSPI), подпись образа (с графом и без) и чип, у которого
//! стёртый флеш читается нулями, два протокола поверх UART (Modbus и protobuf)
//! CANopen на FDCAN и bxCAN, Modbus TCP (с master), protobuf UDP, protobuf и
//! Modbus по USB (Modbus без master — по UART и по USB); чипы с клоком RNG через мультиплексор (G0, WL, WBA, L4) и по одному
//! чипу на каждый вариант регистров `banks.rs` (`g0`, `g4`, `l4`, `l5`), а
//! также несколько транспортов разом (`links`: два UART и TCP, два USB с графом
//! и без, TCP и UDP, UART и CAN, UART и UDP без графа) и транспорты без протокола
//! (UDP, USB с графом). Точный список — в `default_cases()`. Свой набор чипов —
//! позиционными аргументами; `--define k=v` (многократно) добавляет определение
//! к каждой конфигурации, например `--ci gitlab --define config=yes stm32f407ve`.

use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    env,
    fmt::Display,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Instant,
};

use anyhow::{Context, bail};

/// Сколько конфигураций собирается одновременно. Сборка цепочки проектных
/// крейтов (`domain` → `adapters` → `app`) ядра почти не загружает, поэтому
/// параллельные конфигурации окупаются; каждая полоса — свой каталог сборки
/// (гигабайты), так что больше трёх по умолчанию не берём.
const DEFAULT_JOBS: usize = 3;

/// Каждая запись здесь включает свою ветку генерации — см. doc-комментарий
/// выше. Менять только вместе с пониманием, какую ветку теряем.
///
/// Второе поле — суффикс имени каталога: он нужен там, где один и тот же чип
/// проверяется в разных конфигурациях, иначе прогоны затирали бы друг друга.
/// Третье — дополнительные `--define`.
///
/// `.smoke()` — набор `--smoke`: по конфигурации на ось генерации (OTA с
/// подписью и графом, без графа на M0+, 2 KiB RAM, внешняя OTA, CANopen с
/// настройками, сеть на H7, USB, несколько транспортов без графа).
fn default_cases() -> Vec<Case> {
    vec![
        // Одноядерный с OTA и подписью образа (она всегда идёт вместе с OTA:
        // проверка в bootloader'е `screen_update`, ключ через `boot/build.rs`,
        // `salty` со своей `slow-motion` и профилем dev, в `app/src/version.rs`
        // — `FW_VERSION` для `Mode::Signed`), одна карта памяти — самый
        // обычный случай.
        Case::new("stm32f407ve").smoke(),
        // Тот же чип, но от OTA отказались при генерации: ветка «один образ на
        // весь flash» на чипе, куда схема прекрасно помещается.
        Case::new("stm32f407ve").variant("no-ota", &["ota=no"]),
        // Несколько карт памяти: в crates-cross/Cargo.toml должна появиться фича
        // "single-bank", без неё build.rs embassy-stm32 паникует.
        Case::new("stm32f429zg"),
        // Банк 2 за разрывом (H743ZG: банк 2 с 0x0810_0000): `boot` и `bsp`
        // разбирают флеш на `Bank1Region`/`Bank2Region`, `DFU` пишется через
        // второй, подпись проверяется над обоими банками (`screen` со своей
        // веткой). Без `config=yes`: с настройками во flash на этом чипе
        // OTA не помещается (`config_ota = false`), `boot` в проект не входит,
        // и ветка `dfu_bank` не собиралась бы вовсе.
        Case::new("stm32h743zg").variant("dfu-bank", &[]),
        // С завода двухбанковый флеш при одобанковой раскладке: `bsp/src/banks.rs`
        // (вариант `g4`) в `bsp`, его зовут и `Board::new`, и `boot`; `unstable-pac` — у `bsp`.
        Case::new("stm32g474re"),
        // OTA не помещается: crates-cross/boot удаляется из проекта, members и xtask
        // должны это пережить.
        //
        // Заодно — настройки на внешней флеши (`ext_flash=yes`, `ota=no`): драйвер
        // W25Q над шиной-заглушкой, раздел с начала микросхемы. Без OTA общий
        // объект `Flash` при этом не нужен вовсе (`shared_flash = false`), и
        // это единственная конфигурация, где bsp собирается без него, а
        // `config::new` — без аргумента. H7 — потому что именно у него раздел
        // во внутреннем флеше дороже всего (2×128 KiB).
        Case::new("stm32h723ve").variant(
            "config-external",
            &["ext_flash=yes", "config=yes", "ota=no"],
        ),
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
        Case::new("stm32l011f4").smoke(),
        // Раздел настроек вместе с OTA на равных секторах (2 KiB): в хвосте
        // flash, страница раздела — страница чипа, схема OTA после него всё ещё
        // помещается — в проект попадают разом bootloader, ota.rs и config.rs.
        Case::new("stm32g071rb").variant("config", &["config=yes"]),
        // Он же без OTA: две последние страницы flash (таблица векторов
        // приложения стоит в начале). Ветка «config есть, bootloader и ota.rs
        // удалены» — только здесь.
        Case::new("stm32g071rb").variant("config-no-ota", &["config=yes", "ota=no"]),
        // Раздел настроек вместе с OTA на неравных секторах: у F407 он ложится
        // в сектора 2–3 по 16 KiB перед ACTIVE, `STATE` — в сектор 4 (64 KiB);
        // `CONFIG_ERASE_SIZE` и `STATE_ERASE_SIZE` меньше страницы чипа, оба
        // раздела — над `ErasePage`. В проект попадают разом bootloader, ota.rs и
        // config.rs.
        Case::new("stm32f407ve").variant("config", &["config=yes"]),
        // OTA с `STATE`/`DFU` на внешней флеши (`ext_flash=yes`) — на чипе, куда
        // внутренняя схема не помещается вовсе. Разом: слот драйвера в `boot`
        // (блокирующий, с запасным выходом `BootCurrent`) и в `bsp::external`
        // (асинхронный, общий мьютекс), проверка подписи над внешними
        // разделами, настройки на той же
        // микросхеме за `DFU` (`CONFIG_START`/`CONFIG_END` — смещения от её начала), и
        // `shared_flash = false` при OTA.
        Case::new("stm32h723ve")
            .variant("ota-external", &["ext_flash=yes", "config=yes"])
            .smoke(),
        // Внешняя OTA без настроек и без графа: на F4 зона bootloader'а —
        // 128 KiB сектор, `boot` без сторожа, а `embedded-storage` ему нужен
        // только ради геометрии драйвера.
        Case::new("stm32f407ve").variant("ota-external-no-graph", &["ext_flash=yes", "graph=no"]),
        // Флеш, стирающийся в ноль, с внешней OTA: `flash-erase-zero` у
        // `embassy-boot` выключен — `STATE` на внешней NOR, а она стирается в
        // единицы.
        Case::new("stm32l073rz").variant("ota-external", &["ext_flash=yes"]),
        // Шина внешней флеши по чипу: у L073 — SPI (`SpiAdapter` в рецепте), у
        // H723 выше — OCTOSPI за мультиплексором, у L476 — QUADSPI. Заглушка
        // шины при этом объявляет четыре линии, и драйвер собирается с
        // quad-командами.
        Case::new("stm32l476rg").variant("ota-external", &["ext_flash=yes"]),
        // OTA без графа: узел OTA запускает сам `main` (`ota_task`;
        // `app/src/version.rs` есть, `graph.rs` нет), а bootloader — без сторожа.
        // Чип — Cortex-M0+: проверка подписи (она есть всегда, когда есть OTA)
        // живёт в bootloader'е, и код `salty` под Thumb-1 заметно толще, чем
        // под M4; влезает ли release-bootloader в свою зону на M0+, проверяет эта
        // конфигурация.
        Case::new("stm32g071rb").variant("no-graph", &["graph=no"]).smoke(),
        // Единственная ветка, где стёртый флеш читается нулями: у L0/L1 хук
        // включает `flash-erase-zero` у embassy-boot, иначе bootloader читает
        // признак «стёрто» наоборот. Взят чип с запасом flash — на 64 KiB
        // раздел ACTIVE выходит меньше минимальной debug-прошивки (см. там же
        // известные ограничения).
        //
        // Заодно — настройки во внутренней EEPROM (`config=yes` на L0/L1):
        // `adapters::eeprom::Eeprom` поверх `Flash::eeprom_*`, единственное место
        // под настройки на флеше, стирающемся в ноль.
        Case::new("stm32l073rz").variant("config", &["config=yes"]),
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
            &["protocol=canopen", "interface=can", "config=yes"],
        )
        .smoke(),
        // То же на внешней флеши (`ext_flash=yes`, OTA там же): у ошибки её
        // драйвера свой `defmt::Format`.
        Case::new("stm32f407ve").variant(
            "canopen-config-external",
            &[
                "protocol=canopen",
                "interface=can",
                "ext_flash=yes",
                "config=yes",
            ],
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
        )
        .smoke(),
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
        Case::new("stm32g474re")
            .variant("protobuf-usb", &["protocol=protobuf", "interface=usb"])
            .smoke(),
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
                "config=yes",
                "graph=no",
            ],
        ),
        // Несколько транспортов (`links`): у каждого своя группа ресурсов,
        // подмодуль драйвера в `bsp` и канала в `app`, поле `Board` и узел
        // графа (без графа — модуль задачи). Два UART
        // одного вида рядом с сетью (H7: три узла Modbus и один `NET`), два USB
        // (два устройства с разными серийными номерами; без графа — и по
        // сигналу готовности OTA на каждое), TCP и UDP на одном стеке
        // (`NET_SOCKETS` = 1 + 2), поток с протоколом рядом с шиной без протокола,
        // UART с UDP без графа. Транспорты без протокола: UDP (сокет и
        // `PacketMetadata` ждут ваш узел) и USB с графом (поток CDC в
        // `Board` никто не забирает).
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
        )
        .smoke(),
        // REST API (`domain::http`, пул `HTTP[2]`): с графом и OTA — рядом с
        // Modbus по TCP, чтобы номер клиента OTA шёл следом за транспортом и
        // сокетов стало три; без графа и без OTA — пул задач, сведения без
        // protobuf и роутер без `/api/reboot`.
        Case::new("stm32h743zg").variant(
            "rest-modbus-tcp",
            &["links=tcp:modbus", "net_services=rest"],
        ),
        Case::new("stm32f407ve").variant(
            "rest-no-graph-no-ota",
            &["links=tcp", "net_services=rest", "graph=no", "ota=no"],
        ),
        // Консоль SSH (`domain::ssh`, `getrandom` над RNG платы): с графом —
        // вход по паролю и ключ хоста, созданный платой в разделе настроек
        // (`Stored`); без графа — вместе с REST (клиенты OTA 0 и 1), вход по
        // ключу и ключ хоста из файла (`Fixed`). Обе службы с OTA в 256K не
        // влезают — там `ext_flash=yes` (`ACTIVE` 384K).
        Case::new("stm32h743zg").variant(
            "ssh-password-config",
            &["links=tcp", "net_services=ssh", "ssh_auth=password", "config=yes"],
        ),
        Case::new("stm32h743zg").variant(
            "rest-ssh-key-no-graph",
            &[
                "links=tcp",
                "net_services=rest,ssh",
                "ssh_auth=key",
                "ssh_key=ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
                "graph=no",
                "ext_flash=yes",
            ],
        ),
        // Клиент MQTT (`domain::mqtt`, `adapters::net::TcpClient`): с графом и
        // OTA — рядом с Modbus по TCP (клиент OTA 1, третий сокет); без графа и
        // без OTA — задача из `tasks/mqtt.rs`; со всеми службами — номера OTA
        // REST 0–1, SSH 2, MQTT 3 и сокеты каждой; поверх одного UDP —
        // TCP-клиент без TCP-транспорта.
        Case::new("stm32h743zg").variant(
            "mqtt-modbus-tcp",
            &["links=tcp:modbus", "net_services=mqtt"],
        ),
        Case::new("stm32f407ve").variant(
            "mqtt-no-graph-no-ota",
            &["links=tcp", "net_services=mqtt", "graph=no", "ota=no"],
        ),
        Case::new("stm32h743zg").variant(
            "rest-ssh-mqtt",
            &[
                "links=tcp",
                "net_services=rest,ssh,mqtt",
                "ssh_auth=password",
                "ext_flash=yes",
            ],
        ),
        // Без графа, но с OTA: ветка `tasks/mqtt.rs` с клиентом OTA.
        Case::new("stm32f407ve").variant(
            "mqtt-no-graph",
            &["links=tcp", "net_services=mqtt", "graph=no"],
        ),
        Case::new("stm32f407ve").variant("mqtt-udp", &["links=udp", "net_services=mqtt"]),
        // С графом, но без OTA: узел MQTT без клиента OTA, `cmd/reboot` — строка
        // в лог.
        Case::new("stm32f407ve").variant(
            "mqtt-no-ota",
            &["links=tcp", "net_services=mqtt", "ota=no"],
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
    /// Входит в набор `--smoke`.
    smoke: bool,
}

impl Case {
    fn new(chip: &str) -> Self {
        Self {
            chip: chip.to_owned(),
            suffix: None,
            defines: Vec::new(),
            smoke: false,
        }
    }

    fn variant(self, suffix: &str, defines: &[&str]) -> Self {
        Self {
            suffix: Some(suffix.to_owned()),
            defines: defines.iter().map(|d| (*d).to_owned()).collect(),
            ..self
        }
    }

    fn smoke(self) -> Self {
        Self {
            smoke: true,
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
    /// Сколько конфигураций идёт одновременно (`--jobs`).
    jobs: usize,
}

/// Статическая RAM сборки и оценка хука (у ручной раскладки её нет).
type Ram = (u64, Option<u64>);

/// Общее на весь прогон: что проверяем, где и что уже проверено.
struct Run {
    options: Options,
    /// Облегчённая копия шаблона, из которой генерируем.
    template: PathBuf,
    work_dir: PathBuf,
    /// Отпечаток хостового workspace ([`host_key`]) → конфигурация, на которой
    /// он уже прошёл `xtask lint`, `test host` и `deny` корня.
    host_checked: Mutex<HashMap<u64, String>>,
    /// Упала хотя бы одна конфигурация — новых не начинаем.
    failed: AtomicBool,
    finished: AtomicUsize,
}

fn main() -> anyhow::Result<()> {
    let options = parse_args()?;
    let repo_root = repo_root();

    // Вне репозитория, и это важнее удобства: `cargo generate --path` копирует
    // каталог шаблона ЦЕЛИКОМ во временную папку, прежде чем прочитать
    // cargo-generate.toml, так что `ignore = ["target", ...]` от копирования
    // не спасает — он решает лишь, что попадёт в готовый проект. Кеш сборки
    // (каталог на полосу, `target-<N>`) дорастает до гигабайтов;
    // внутри `target/` он превращал каждую генерацию из локального пути в
    // копирование этих гигабайтов. Однажды это кончилось «Недостаточно места
    // на диске (os error 112)» на ровном месте.
    let work_dir = work_dir(&repo_root);
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("создать рабочий каталог {}", work_dir.display()))?;

    // Каталог постоянный, чтобы кеш сборки оставался тёплым между прогонами, —
    // значит, два прогона на одной рабочей копии затирали бы друг другу
    // проекты. Блокировку ОС снимает и падение процесса, «зависшей» не бывает.
    let lock_path = work_dir.join("lock");
    // Без усечения: открытие на усечение файла, который держит другой процесс,
    // на Windows может упасть раньше `try_lock` с невнятной ошибкой.
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("открыть {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => bail!(
            "{} занят другим прогоном template-check на этой же рабочей копии. Дождитесь его \
             или задайте свой каталог: TEMPLATE_CHECK_DIR=C:\\tc-<имя> (путь короткий — \
             иначе генерация падает на `git init` с `path too long`)",
            work_dir.display(),
        ),
        Err(fs::TryLockError::Error(err)) => {
            return Err(err).with_context(|| format!("заблокировать {}", lock_path.display()));
        }
    }

    // Логи прошлого прогона — прочь: у конфигураций, до которых этот не
    // дойдёт, старый лог выдавал бы себя за нынешний.
    let logs = work_dir.join("logs");
    if logs.exists() {
        fs::remove_dir_all(&logs).with_context(|| format!("очистить {}", logs.display()))?;
    }
    fs::create_dir_all(&logs).with_context(|| format!("создать {}", logs.display()))?;

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
    let lanes = lanes(&options.cases, options.jobs);
    println!(
        "Проверяем шаблон на {} конфигурациях (ci={}{}, полос: {}): {}\nлоги: {}",
        options.cases.len(),
        options.ci,
        if options.quick { ", --quick" } else { "" },
        lanes.len(),
        labels.join(", "),
        work_dir.join("logs").display(),
    );

    let run = Run {
        options,
        template,
        work_dir,
        host_checked: Mutex::new(HashMap::new()),
        failed: AtomicBool::new(false),
        finished: AtomicUsize::new(0),
    };
    let mut outcomes: Vec<Option<anyhow::Result<Option<Ram>>>> =
        run.options.cases.iter().map(|_| None).collect();
    thread::scope(|scope| {
        let run = &run;
        let handles: Vec<_> = lanes
            .into_iter()
            .enumerate()
            .map(|(lane, cases)| {
                // Свой каталог сборки на полосу: cargo держит каталог под
                // блокировкой всю сборку, общий выстроил бы полосы в очередь.
                let target_dir = run.work_dir.join(format!("target-{lane}"));
                scope.spawn(move || run_lane(run, &cases, &target_dir))
            })
            .collect();
        for handle in handles {
            for (index, outcome) in handle.join().expect("полоса проверки запаниковала")
            {
                outcomes[index] = Some(outcome);
            }
        }
    });

    let mut failed = 0;
    let mut static_ram = Vec::new();
    for (case, outcome) in run.options.cases.iter().zip(outcomes) {
        match outcome {
            Some(Ok(Some(ram))) => static_ram.push((case.label(), ram)),
            Some(Ok(None)) => {}
            Some(Err(err)) => {
                failed += 1;
                let log = log_path(&run.work_dir, case);
                eprintln!("\n=== {}: {err:#}", case.label());
                eprintln!("--- хвост лога {} ---\n{}", log.display(), log_tail(&log));
            }
            None => println!(
                "{}: не запускалась — прогон остановлен падением",
                case.label()
            ),
        }
    }

    // Факт статической RAM по случаям — из строки `ram:` вывода `build`.
    if !static_ram.is_empty() {
        println!("\nСтатическая RAM (.data + .bss + .uninit), байт: факт и оценка хука");
        let width = static_ram
            .iter()
            .map(|(label, _)| label.chars().count())
            .max()
            .unwrap_or(0);
        for (label, (ram, estimate)) in &static_ram {
            let estimate = estimate.map_or_else(|| "—".to_owned(), |e| e.to_string());
            println!("  {label:<width$}  {ram:>7}  {estimate:>7}");
        }
    }

    if failed > 0 {
        bail!("упало конфигураций: {failed} (причины и хвосты логов — выше)");
    }
    println!(
        "\nГотово: {} конфигураций прошли {}.",
        run.options.cases.len(),
        if run.options.quick {
            "генерацию"
        } else {
            "генерацию, lint и сборку"
        },
    );
    Ok(())
}

/// Рабочий каталог: `TEMPLATE_CHECK_DIR`, если задан, иначе постоянный каталог
/// во временной папке, свой у каждой рабочей копии (у worktree — свой шаблон).
fn work_dir(repo_root: &Path) -> PathBuf {
    if let Some(dir) = env::var_os("TEMPLATE_CHECK_DIR") {
        return PathBuf::from(dir);
    }
    // Хешер меняется с тулчейном — тогда кеш один раз остынет, только и всего.
    let mut hasher = DefaultHasher::new();
    repo_root.hash(&mut hasher);
    env::temp_dir().join(format!("rtc-{:016x}", hasher.finish()))
}

fn log_path(work_dir: &Path, case: &Case) -> PathBuf {
    work_dir.join("logs").join(format!("{}.log", case.name()))
}

/// Последние строки лога упавшей конфигурации — чтобы причина была видна без
/// открытия файла.
fn log_tail(log: &Path) -> String {
    const LINES: usize = 40;
    let text = fs::read(log).map_or_else(
        |err| format!("(не прочитать: {err})"),
        |bytes| String::from_utf8_lossy(&bytes).into_owned(),
    );
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(LINES)..].join("\n")
}

/// Раскладка конфигураций по полосам (индексы в `cases`). Конфигурации одного
/// чипа — в одну полосу: `embassy-stm32` под чип собирается на её каталоге
/// сборки один раз, а проверки `chip-info` и заготовок ([`ChipChecks`]) не
/// повторяются. Крупные группы раскладываются первыми, каждая — в наименее
/// загруженную полосу.
fn lanes(cases: &[Case], jobs: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        match groups.iter_mut().find(|(chip, _)| *chip == case.chip) {
            Some((_, group)) => group.push(index),
            None => groups.push((&case.chip, vec![index])),
        }
    }
    groups.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
    let mut lanes = vec![Vec::new(); jobs.clamp(1, groups.len().max(1))];
    for (_, group) in groups {
        lanes
            .iter_mut()
            .min_by_key(|lane| lane.len())
            .expect("полос не меньше одной")
            .extend(group);
    }
    lanes
}

/// Конфигурации полосы по очереди, на одном каталоге сборки.
fn run_lane(
    run: &Run,
    cases: &[usize],
    target_dir: &Path,
) -> Vec<(usize, anyhow::Result<Option<Ram>>)> {
    let mut chip_checks = ChipChecks::default();
    let mut outcomes = Vec::new();
    for &index in cases {
        if run.failed.load(Ordering::Relaxed) {
            break;
        }
        let case = &run.options.cases[index];
        let log_path = log_path(&run.work_dir, case);
        let started = Instant::now();
        let outcome = File::create(&log_path)
            .with_context(|| format!("создать лог {}", log_path.display()))
            .and_then(|log| check_one(run, target_dir, case, &log, &mut chip_checks));
        let finished = run.finished.fetch_add(1, Ordering::Relaxed) + 1;
        let verdict = if outcome.is_ok() {
            "ok  "
        } else {
            run.failed.store(true, Ordering::Relaxed);
            "FAIL"
        };
        println!(
            "[{finished}/{}] {verdict} {} — {} с",
            run.options.cases.len(),
            case.label(),
            started.elapsed().as_secs(),
        );
        outcomes.push((index, outcome));
    }
    outcomes
}

/// Какие чипы полосы уже прошли проверки `chip-info` и заготовок. От проекта
/// они не зависят — только от чипа; исключение — половина `assign_resources!`,
/// которая проверяется лишь в проекте без интерфейса (см.
/// [`check_snippets_compile`]), поэтому такой проект проверяет чип ещё раз.
#[derive(Default)]
struct ChipChecks {
    any: HashSet<String>,
    with_resources: HashSet<String>,
}

impl ChipChecks {
    /// Нужны ли проверки чипа этому проекту; если да — считает их сделанными.
    fn due(&mut self, chip: &str, placeholder_resources: bool) -> bool {
        if self.with_resources.contains(chip) || (!placeholder_resources && self.any.contains(chip))
        {
            return false;
        }
        self.any.insert(chip.to_owned());
        if placeholder_resources {
            self.with_resources.insert(chip.to_owned());
        }
        true
    }
}

/// Команды, которые зависят только от чипа (`chip-info`), — раз на чип полосы.
///
/// `xtask pins` — единственный способ вообще проверить `chip-info`: его
/// манифест до генерации не резолвится (Liquid в чип-фиче), так что ни `cargo
/// xtask lint`, ни CI самого шаблона его не трогают. Оба вызова не случайны:
/// без аргумента идёт перечисление блоков, с `RCC` — ветка с таблицей выводов,
/// DMA и тактированием, а `RCC` есть на любом STM32 (в отличие от, например,
/// `SPI4`).
///
/// fmt/clippy для chip-info — здесь же и по той же причине; в сгенерированном
/// проекте манифест уже подставлен, и обе команды работают. `cargo xtask lint`
/// их намеренно не зовёт: тогда каждый пользовательский CI тянул бы
/// stm32-metapac ради инструмента, который на сборку прошивки никак не влияет.
const CHIP_COMMANDS: [&[&str]; 5] = [
    &["xtask", "pins"],
    &["xtask", "pins", "RCC"],
    // Заготовка — на I2C1, и это не произвольный выбор: он есть на всех чипах
    // набора (у stm32l011f4, например, USART1 нет вовсе — только USART2 и
    // LPUART1), а обработчиков прерывания у него два и они разные, то есть
    // проверяется самая ошибкоопасная ветка таблицы.
    &["xtask", "pins", "I2C1", "--snippet"],
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

/// Строка в лог конфигурации.
fn note(mut log: &File, line: impl Display) -> anyhow::Result<()> {
    writeln!(log, "{line}").context("записать в лог")
}

/// Отдаёт статическую RAM сборки и оценку хука (без сборки — `None`).
fn check_one(
    run: &Run,
    cargo_target_dir: &Path,
    case: &Case,
    log: &File,
    chip_checks: &mut ChipChecks,
) -> anyhow::Result<Option<Ram>> {
    let options = &run.options;
    // Имя проекта = имя каталога, в который cargo-generate его положит.
    let name = case.name();
    let project = run.work_dir.join(&name);
    if project.exists() {
        fs::remove_dir_all(&project)
            .with_context(|| format!("очистить прошлый прогон {}", project.display()))?;
    }

    note(log, format!("=== {} ===", case.label()))?;
    let mut command = Command::new("cargo");
    command
        .arg("generate")
        .arg("--path")
        .arg(&run.template)
        .arg("--name")
        .arg(&name)
        .arg("--define")
        .arg(format!("chip_feature={}", case.chip))
        .arg("--define")
        .arg(format!("ci={}", options.ci));
    for define in case.defines.iter().chain(&options.defines) {
        command.arg("--define").arg(define);
    }
    // Вывод генерации перехватывается ради строки `ram_estimate:` хука.
    let generated = capture(
        command
            .arg("--silent")
            .arg("--destination")
            .arg(&run.work_dir),
        &run.work_dir,
        None,
        log,
    )
    .with_context(|| format!("генерация проекта под {}", case.label()))?;
    note(log, &generated)?;
    // С ручной раскладкой оценки нет — и сверять нечего.
    let estimate = parse_ram_estimate(&generated);

    check_no_raw_placeholders(&project, log)?;
    check_ci_files(&project, &options.ci, log)?;

    let mut static_ram = None;
    if !options.quick {
        // До первой команды: дальше в проекте появляются `Cargo.lock` и секреты.
        let host_key = host_key(&project, case)?;
        // Ровно те команды, которые README обещает пользователю шаблона.
        let cargo = |args: &[&str]| {
            run_command(
                Command::new("cargo").args(args),
                &project,
                Some(cargo_target_dir),
                log,
            )
            .with_context(|| {
                format!(
                    "`cargo {}` в сгенерированном проекте (оставлен для разбора: {})",
                    args.join(" "),
                    project.display(),
                )
            })
        };

        let resources = project.join("crates-cross/bsp/src/resources.rs");
        let placeholder_resources = is_placeholder_resources(
            &fs::read_to_string(&resources)
                .with_context(|| format!("прочитать {}", resources.display()))?,
        );
        if chip_checks.due(&case.chip, placeholder_resources) {
            check_snippets_compile(&project, cargo_target_dir, log)?;
            for args in CHIP_COMMANDS {
                cargo(args)?;
            }
        } else {
            note(
                log,
                "chip-info и заготовки этого чипа уже проверены в полосе — пропуск",
            )?;
        }

        // Хостовый workspace от чипа и `crates-cross` не зависит: одинаковый
        // по содержимому проверяется один раз на прогон.
        // Отпечаток записывается, только когда проверки прошли (после `deny`
        // ниже): иначе соседняя полоса пропустила бы их, пока они ещё идут, и
        // отчиталась бы «ok» за непройденные. Цена — изредка повтор, если две
        // полосы дошли до одного отпечатка одновременно.
        let checked_on = run
            .host_checked
            .lock()
            .expect("под мьютексом отпечатков не паникуют")
            .get(&host_key)
            .cloned();
        if let Some(label) = &checked_on {
            note(
                log,
                format!(
                    "хостовый workspace совпадает с {label} — `xtask lint`, `test host` и \
                     `deny` корня пропущены",
                ),
            )?;
        }
        let host_due = checked_on.is_none();
        if host_due {
            cargo(&["xtask", "lint"])?;
            cargo(&["xtask", "test", "host"])?;
        }
        cargo(&["xtask", "lint", "cross"])?;

        // Вывод `build` перехватывается ради строки `ram:` и пишется в лог
        // как есть, чтобы отчёт сборки остался.
        let output = capture(
            Command::new("cargo").args(["xtask", "build"]),
            &project,
            Some(cargo_target_dir),
            log,
        )
        .with_context(|| {
            format!(
                "`cargo xtask build` в сгенерированном проекте (оставлен для разбора: {})",
                project.display(),
            )
        })?;
        note(log, &output)?;
        let ram = parse_static_ram(&output).with_context(|| {
            format!(
                "в выводе `cargo xtask build` нет строки `ram: <N> байт статической RAM …` \
                 (формат — ram_line в crates-host/xtask/src/size.rs) для {}",
                case.label(),
            )
        })?;
        // Оценка хука консервативна: ниже факта — таблица `RAM_COST`
        // устарела.
        if let Some(estimate) = estimate {
            if estimate < ram {
                bail!(
                    "{}: оценка RAM хука {estimate} байт ниже факта {ram} байт — \
                     перемерьте RAM_COST в chip-select.rhai (MAINTAINING.md, «Оценка RAM»)",
                    case.label(),
                );
            }
            note(
                log,
                format!("ram: оценка {estimate} байт не ниже факта {ram} байт"),
            )?;
        }
        static_ram = Some((ram, estimate));

        // Лицензии, источники, уязвимости: и хостовый workspace, и прошивка (у
        // неё свой `Cargo.lock`, он появляется после `build`).
        if host_due {
            cargo(&["deny", "check"])?;
            run.host_checked
                .lock()
                .expect("под мьютексом отпечатков не паникуют")
                .insert(host_key, case.label());
        }
        cargo(&[
            "deny",
            "--manifest-path",
            "crates-cross/Cargo.toml",
            "check",
        ])?;
        // От проекта зависит: `--check` на нетронутом `bsp` обязан проходить —
        // шаблон не занимает ни одного вывода, а выбранный интерфейс — свои.
        cargo(&["xtask", "pins", "--check"])?;
    }

    if options.keep {
        note(log, format!("проект оставлен: {}", project.display()))?;
    } else {
        fs::remove_dir_all(&project).with_context(|| format!("удалить {}", project.display()))?;
    }
    Ok(static_ram)
}

/// `resources.rs` — заглушка без групп: проект без интерфейса.
fn is_placeholder_resources(resources: &str) -> bool {
    resources.contains("BoardResources {}")
}

/// Что хостовый workspace (`xtask lint`, `test host`, `deny` корня) не читает —
/// пути от корня проекта. Список исключений, а не включений: забытое здесь
/// стоит лишь лишнего прогона, а забытое во включениях — пропущенной проверки.
const NOT_HOST: &[&str] = &[
    ".git",
    ".github",
    ".vscode",
    ".claude",
    "target",
    "docs",
    "crates-cross",
    "crates-host/chip-info",
];

/// Отпечаток хостового workspace проекта: все файлы, кроме [`NOT_HOST`] и
/// `.md`, с именем проекта и чип-фичей, заменёнными на одно и то же. Они стоят
/// в строковых литералах и на исход проверок не влияют, а без замены
/// отпечатки разных проектов не совпадали бы никогда.
fn host_key(project: &Path, case: &Case) -> anyhow::Result<u64> {
    let mut files = Vec::new();
    visit_files(project, &mut |path| {
        let relative = path
            .strip_prefix(project)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let skipped = relative.ends_with(".md")
            || NOT_HOST.iter().any(|dir| {
                relative
                    .strip_prefix(dir)
                    .is_some_and(|rest| rest.starts_with('/'))
            });
        if !skipped {
            files.push(relative);
        }
        Ok(())
    })?;
    files.sort();

    // Имя — раньше чипа: оно его содержит.
    let name = case.name();
    let aliases = [name.replace('-', "_"), name, case.chip.clone()];
    let mut hasher = DefaultHasher::new();
    for relative in files {
        let bytes =
            fs::read(project.join(&relative)).with_context(|| format!("прочитать {relative}"))?;
        let text = aliases.iter().fold(
            String::from_utf8_lossy(&bytes).to_lowercase(),
            |text, alias| text.replace(alias.as_str(), "\u{0}"),
        );
        relative.hash(&mut hasher);
        text.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

/// Ищет `{{ }}`/`{% %}`, пережившие генерацию. Мимо линта и сборки такое
/// проходит незамеченным везде, кроме `.rs`/`.toml`: `.json`, `.yml` и
/// `memory.x` никто не компилирует, и сломанный `launch.json` обнаружился бы
/// только у пользователя.
fn check_no_raw_placeholders(project: &Path, log: &File) -> anyhow::Result<()> {
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
    note(log, "плейсхолдеры: не осталось ни одного")
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
fn check_ci_files(project: &Path, ci: &str, log: &File) -> anyhow::Result<()> {
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
    note(log, format!("файлы CI при ci={ci}: как ожидалось"))
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

/// Оценка статической RAM из вывода хука — строка `ram_estimate: <N> байт …`,
/// которую печатает `chip-select.rhai` (раздел «RAM»). Нет строки — ручная
/// раскладка, оценки нет.
fn parse_ram_estimate(generate_output: &str) -> Option<u64> {
    generate_output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("ram_estimate: ")?
            .split_once(" байт")?
            .0
            .parse()
            .ok()
    })
}

/// То же, что [`run_command`], но отдаёт stdout: нужно там, где проверяется не
/// код возврата, а напечатанное. stderr успешной команды уходит в лог:
/// прогресс и предупреждения cargo не теряются, как и у [`run_command`].
fn capture(
    command: &mut Command,
    current_dir: &Path,
    cargo_target_dir: Option<&Path>,
    mut log: &File,
) -> anyhow::Result<String> {
    command.current_dir(current_dir);
    if let Some(target_dir) = cargo_target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let output = command
        .output()
        .with_context(|| format!("запустить {:?}", command.get_program()))?;
    if !output.status.success() {
        // stdout — тоже: перед ошибкой хук печатает то, что к ней привело.
        bail!(
            "команда завершилась с {}:\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
    log.write_all(&output.stderr).context("записать в лог")?;
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
fn check_snippets_compile(
    project: &Path,
    cargo_target_dir: &Path,
    log: &File,
) -> anyhow::Result<()> {
    let listing = capture(
        Command::new("cargo").args(["xtask", "pins"]),
        project,
        Some(cargo_target_dir),
        log,
    )?;
    // `cargo xtask pins` выше собрал `chip-info`; дальше — он сам, без двух
    // вложенных `cargo run` на каждый блок (их набиралось больше сотни на
    // проект). Обёртку `xtask pins` проверяют [`CHIP_COMMANDS`].
    let chip_info = cargo_target_dir
        .join("debug")
        .join(format!("chip-info{}", env::consts::EXE_SUFFIX));
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
    let placeholder_only = is_placeholder_resources(&original_resources);

    let mut resources = String::new();
    let mut module = String::from(
        "//! Временный модуль: сюда `template-check` складывает заготовки из\n\
         //! `cargo xtask pins <БЛОК> --snippet` и компилирует их. В проекте\n\
         //! пользователя этого файла нет — он создаётся и удаляется проверкой.\n\
         #![allow(dead_code, reason = \"заготовки никто не вызывает\")]\n\n",
    );
    for block in &blocks {
        let snippet = capture(
            Command::new(&chip_info).args([block.as_str(), "--snippet"]),
            project,
            None,
            log,
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

    let checked = run_command(
        Command::new("cargo").args(["clippy", "-p", "bsp", "--", "-D", "warnings"]),
        &project.join("crates-cross"),
        Some(cargo_target_dir),
        log,
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
    note(
        log,
        format!("заготовки: {} блоков компилируются", blocks.len()),
    )
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

/// Запускает команду с выводом в лог конфигурации.
fn run_command(
    command: &mut Command,
    current_dir: &Path,
    cargo_target_dir: Option<&Path>,
    log: &File,
) -> anyhow::Result<()> {
    command.current_dir(current_dir);
    if let Some(target_dir) = cargo_target_dir {
        // Общий каталог сборки полосы: то, что от чипа не зависит (domain и
        // весь host-граф), собирается в ней один раз на прогон.
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    command
        .stdout(log.try_clone().context("открыть лог для команды")?)
        .stderr(log.try_clone().context("открыть лог для команды")?);
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
    let mut smoke = false;
    let mut only: Vec<String> = Vec::new();
    let mut jobs = DEFAULT_JOBS;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => quick = true,
            "--keep" => keep = true,
            "--smoke" => smoke = true,
            "--only" => only.extend(
                args.next()
                    .context("--only требует значения (подстроки меток через запятую)")?
                    .split(',')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned),
            ),
            "--jobs" => {
                jobs = args
                    .next()
                    .context("--jobs требует числа")?
                    .parse()
                    .context("--jobs требует числа")?;
            }
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
                    "USAGE: template-check [--quick] [--smoke] [--only a,b] [--jobs N] [--keep] \
                     [--ci github|gitlab|none] [--define k=v ...] [chip_feature ...]\n\n\
                     --smoke — малый набор, --only — конфигурации, в метке которых есть одна \
                     из подстрок (вместе — пересечение), --jobs — сколько одновременно \
                     (по умолчанию {DEFAULT_JOBS}).\n\n\
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
        cases = default_cases()
            .into_iter()
            .filter(|case| !smoke || case.smoke)
            .filter(|case| {
                let label = case.label();
                only.is_empty() || only.iter().any(|part| label.contains(part.as_str()))
            })
            .collect();
        anyhow::ensure!(
            !cases.is_empty(),
            "под --smoke/--only {} не подошла ни одна конфигурация (`--help` — список меток)",
            only.join(","),
        );
    } else {
        anyhow::ensure!(
            !smoke && only.is_empty(),
            "--smoke и --only выбирают из набора по умолчанию, а не из перечисленных чипов",
        );
    }
    Ok(Options {
        cases,
        ci,
        defines,
        quick,
        keep,
        jobs,
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
    use super::{
        Case, ChipChecks, lanes, parse_ram_estimate, parse_static_ram, without_actions_expressions,
    };

    /// Чип целиком в одной полосе, крупные группы разнесены по разным.
    #[test]
    fn lanes_keep_a_chip_together_and_balance_by_size() {
        let cases: Vec<Case> = ["a", "b", "a", "c", "a", "b"]
            .into_iter()
            .map(Case::new)
            .collect();

        assert_eq!(lanes(&cases, 2), vec![vec![0, 2, 4], vec![1, 5, 3]]);
        assert_eq!(lanes(&cases, 10).len(), 3, "полос не больше, чем чипов");
        assert_eq!(lanes(&cases, 0).len(), 1);
    }

    /// Проект без интерфейса проверяет чип ещё раз (ради `assign_resources!`),
    /// дальше — никто.
    #[test]
    fn chip_checks_rerun_once_for_the_resources_half() {
        let mut checks = ChipChecks::default();

        assert!(checks.due("x", false));
        assert!(!checks.due("x", false));
        assert!(checks.due("x", true));
        assert!(!checks.due("x", true));
        assert!(!checks.due("x", false));
        assert!(checks.due("y", true));
        assert!(!checks.due("y", false));
    }

    /// Строку печатает `crates-host/xtask/src/size.rs` (`ram_line`).
    #[test]
    fn parses_the_static_ram_line_of_a_build() {
        let output = "app: 41.2 KiB из 128.0 KiB ACTIVE (32.2%)\n\
                      ram: 3124 байт статической RAM из 131072 байт RAM (2.4%)\n\
                      образ для OTA: app.bin\n";

        assert_eq!(parse_static_ram(output), Some(3124));
        assert_eq!(parse_static_ram("app: 41.2 KiB\n"), None);
    }

    /// Строку печатает `chip-select.rhai` (раздел «RAM»).
    #[test]
    fn parses_the_ram_estimate_line_of_the_hook() {
        let output = "OTA отключён по вашему выбору\n\
                      ram_estimate: 3932 байт статической RAM (+ 1024 под стек) из 130560 байт RAM\n";
        assert_eq!(parse_ram_estimate(output), Some(3932));
        assert_eq!(parse_ram_estimate("ram: 3124 байт статической RAM\n"), None);
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
