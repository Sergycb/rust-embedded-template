#![cfg_attr(not(test), no_std)]
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
//! [`FirmwareUpdate`] и [`SettingsStorage`] реализованы, когда при генерации
//! выбраны OTA и хранилище настроек; канал доставки образа ([`ImageSource`])
//! — сам проект. Порты протоколов связи (`FrameLink`, `Registers`,
//! `ModbusClient`/`ModbusServer`, `HostService`, `CanBus`) есть только в
//! проекте, где протокол выбран при генерации.
//!
//! Модули названы по операции домена, которая ими пользуется, а не по трейту:
//! ошибка операции (`DownloadError` у `domain::ota::download::receive`) лежит
//! рядом с портом, через который операция идёт. Наружу всё реэкспортируется в
//! корень — путь `ports::FirmwareUpdate` короче, а внутреннее деление
//! пользователя не касается (`docs/conventions.md`, «`lib.rs` только объявляет
//! модули»).

// {%- if protocol == "canopen" %}
/// Шина CAN: кадры для узла CANopen.
pub mod can;

// {%- endif %}
// {%- if protocol == "protobuf" %}
/// Обработчик сообщений хоста: сообщения схемы → типы приложения.
pub mod host;

// {%- endif %}
// {%- if protocol != "none" %}
/// Кадровый канал связи: сообщения целиком, поверх датаграмм или адаптера
/// кадрирования потока.
pub mod link;

// {%- endif %}
// {%- if protocol == "modbus" %}
/// Modbus: данные slave'а, представления битов и регистров кадра, транспорты
/// master'а и slave'а, ошибки обмена.
pub mod modbus;

// {%- endif %}
/// OTA: канал доставки образа, запись во флеш, проверка подписи и ошибки
/// приёма и применения.
pub mod ota;

/// Хранилище настроек.
pub mod settings;

// {%- if protocol == "canopen" %}
pub use can::CanBus;
// {%- endif %}
// {%- if protocol == "protobuf" %}
pub use host::HostService;
// {%- endif %}
// {%- if protocol != "none" %}
pub use link::{FrameLink, LinkError};
// {%- endif %}
// {%- if protocol == "modbus" %}
pub use modbus::{
    Bits, BitsMut, Exception, MAX_PDU, MasterError, ModbusClient, ModbusServer, Received,
    Registers, ServeError, Words, WordsMut,
};
// {%- endif %}
pub use ota::download::{Announce, DownloadError, ImageSource, Rejection};
pub use ota::update::{FirmwareUpdate, PendingImage, UpdateError};
pub use settings::SettingsStorage;
