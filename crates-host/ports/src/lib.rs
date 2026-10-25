#![cfg_attr(not(test), no_std)]
// {%- if ports_async == "true" or net_services contains "mqtt" %}
// `async fn` в публичном трейте: rustc предупреждает (`async_fn_in_trait`), что
// у возвращаемого future нет границы `Send` и потребовать её вызывающий не
// сможет. Для однопоточного исполнителя embassy это ровно то, что нужно, а с
// `-D warnings` предупреждение стало бы ошибкой сборки. Так же поступает
// `embedded-hal-async` (его lib.rs, строка 10).
//
// `#[expect]`, а не `#[allow]`: если линт перестанет срабатывать, компилятор
// сам напомнит убрать атрибут.
#![expect(
    async_fn_in_trait,
    reason = "Embassy tasks require async fn in trait; supervisor fragments call them on a single executor"
)]
// {%- endif %}

//! Порты — границы `domain`: трейты, которые логика вызывает, не зная, что за
//! ними стоит, и типы, которыми она с ними разговаривает. Реализации живут в
//! `adapters` (всё, что generic по `embedded-hal`/`embedded-storage` и потому
//! тестируется на хосте) и в `crates-cross/bsp` (то, чему нужна периферия
//! конкретного чипа), а сама логика ни о тех, ни о других не знает.
//!
//! Правило, по которому решается, что сюда класть: если для проверки логики
//! нужно железо — значит между логикой и железом не хватает порта. Обратное
//! правило не менее важно: порт заводится под то, чем пользуется логика, а не
//! под каждую железную деталь. Причина прошлой паники домену не нужна — это
//! диагностика старта, и её печатает `main` сам.
//!
//! Отсюда и то, чего здесь нет: сведения о чипе (`bsp::ChipSpec`) домен не
//! читает, поэтому их тип живёт в `bsp`, рядом с `Board`.
//!
//! Типы ошибок тоже здесь, а не в `domain`: домен — только функции над портами,
//! а всё, что он возвращает, объявлено в его словаре.
//!
// {%- if ota == "true" %}
//! [`FirmwareUpdate`] реализован, когда при генерации выбрана OTA; образ
//! приходит командами [`OtaCommand`] от узла протокола.
// {%- endif %}
// {%- if settings_port == "true" %}
//! [`SettingsStorage`] — хранилище настроек: его реализует
// {%- if config_store == "eeprom" %}
//! внутренняя EEPROM.
// {%- elsif config_store == "external" %}
//! раздел на внешней флеши.
// {%- elsif config_store == "flash" %}
//! раздел внутреннего флеша.
// {%- else %}
//! носитель платы.
// {%- endif %}
// {%- endif %}
//! Порты протоколов связи и транспортов — только те, что выбраны при
//! генерации.
//!
// {%- if ota == "true" %}
//! Модули названы по операции домена, которая ими пользуется, а не по трейту:
//! ошибка операции (`DownloadError` у `domain::ota::download`) лежит
//! рядом с портом, через который операция идёт. Наружу всё реэкспортируется в
//! корень — путь `ports::FirmwareUpdate` короче, а внутреннее деление
//! пользователя не касается (`docs/conventions.md`, «`lib.rs` только объявляет
//! модули»).
// {%- else %}
//! Наружу всё реэкспортируется в корень, а внутреннее деление пользователя не
//! касается (`docs/conventions.md`, «`lib.rs` только объявляет модули»).
// {%- endif %}

/// Сборка адаптера из рычага платы и памяти приложения.
pub mod assemble;

// {%- if has_can == "true" %}
// {%- if has_canopen == "true" %}
/// Шина CAN: кадры для узла CANopen.
// {%- else %}
/// Шина CAN: кадры для вашего протокола.
// {%- endif %}
pub mod can;

// {%- endif %}
// {%- if has_protobuf == "true" %}
/// Обработчик сообщений хоста: сообщения схемы → типы приложения.
pub mod host;

// {%- endif %}
// {%- if link_port == "true" %}
/// Кадровый канал связи: сообщения целиком, поверх датаграмм или адаптера
/// кадрирования потока.
pub mod link;

// {%- endif %}
// {%- if net_services contains "mqtt" %}
/// Исходящее соединение: клиент подключается к серверу и пережидает отказы.
pub mod dial;

// {%- endif %}
// {%- if has_modbus == "true" %}
/// Modbus: данные slave'а, представления битов и регистров кадра, транспорты,
/// ошибки обмена.
pub mod modbus;

// {%- endif %}
// {%- if ota == "true" or has_canopen == "true" %}
// {%- if ota == "true" and has_canopen == "true" %}
/// Слово записи флеша — общий предел буферов OTA и хранилища CANopen.
// {%- elsif ota == "true" %}
/// Слово записи флеша — предел буферов OTA.
// {%- else %}
/// Слово записи флеша — предел буферов хранилища CANopen.
// {%- endif %}
mod word;

// {%- endif %}
// {%- if core_baseline != "true" %}
/// Счётчик тактов ядра.
pub mod cycles;

// {%- endif %}
// {%- if ota == "true" %}
/// OTA: команды узлу, запись во флеш, проверка подписи, перезагрузка и ошибки
/// приёма и применения.
pub mod ota;

// {%- endif %}
// {%- if settings_port == "true" %}
/// Хранилище настроек.
pub mod settings;
// {%- endif %}
// {%- if config_store == "eeprom" %}

/// Доступ к внутренней EEPROM — под хранилищем настроек на L0/L1.
pub mod eeprom;
// {%- endif %}

pub use assemble::Assemble;
// {%- if has_can == "true" %}
pub use can::CanBus;
// {%- endif %}
// {%- if has_protobuf == "true" %}
pub use host::HostService;
// {%- if ota == "true" %}
pub use host::Route;
// {%- endif %}
// {%- endif %}
// {%- if link_port == "true" %}
pub use link::{FrameDuplex, FrameLink, LinkError};
// {%- if has_modbus == "true" %}
pub use link::Handler;
// {%- endif %}
// {%- endif %}
// {%- if net_services contains "mqtt" %}
pub use dial::Dial;
// {%- endif %}
// {%- if has_modbus == "true" %}
pub use modbus::{Bits, BitsMut, Exception, MAX_PDU, Registers, Words, WordsMut};
// {%- if modbus_master == "true" %}
pub use modbus::{MasterError, ModbusClient};
// {%- endif %}
// {%- endif %}
// {%- if core_baseline != "true" %}
pub use cycles::CycleCounter;
// {%- endif %}
// {%- if ota == "true" %}
pub use ota::command::{
    CHUNK_BYTES, Chunk, Command as OtaCommand, Progress, Reboot, Rejection, Reply as OtaReply,
    SIGNATURE_BYTES, reply_code,
};
pub use ota::download::DownloadError;
pub use ota::update::{
    FirmwareLayout, FirmwarePartitions, FirmwareUpdate, PendingImage, UpdateError,
};
// {%- endif %}
// {%- if settings_port == "true" %}
pub use settings::SettingsStorage;
// {%- endif %}
// {%- if config_store == "eeprom" %}
pub use eeprom::EepromAccess;
// {%- endif %}
// {%- if ota == "true" or has_canopen == "true" %}
pub use word::MAX_WRITE_SIZE;
// {%- endif %}
