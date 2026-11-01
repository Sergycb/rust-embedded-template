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

//! Порты — контракт между `domain` и реализациями: трейты, которые логика
//! вызывает, не зная, что за ними стоит, и то, без чего контракт не выразить, —
//! типы из сигнатур трейтов и форматы, которые обязаны совпадать у реализации и
//! логики. Реализации живут в
//! `adapters` (всё, что generic по `embedded-hal`/`embedded-storage` и потому
//! тестируется на хосте) и в `crates-cross/bsp` (то, чему нужна периферия
//! конкретного чипа), а сама логика ни о тех, ни о других не знает.
//!
//! Когда порт заводится, а когда логика зовёт трейт `embedded-hal` или
//! библиотеку прямо, — docs/architecture.md, «Порт и адаптер — или библиотека
//! прямо в `domain`».
//!
//! Отсюда и то, чего здесь нет: сведения о чипе (`bsp::ChipSpec`) — данные, а
//! не поведение, и их тип живёт в `bsp`, рядом с `Board`. Причина сброса и
//! сообщение прошлой паники приходят в сведения об устройстве строками от `app`.
//!
//! Нет здесь и того, что нужно только логике, — команд узлам, ошибок её
//! операций: они в `domain`, рядом с узлом. Реализация порта их не видит, и
//! правка их в `ports` задевала бы `adapters` и `bsp` зря.
//!
// {%- if ota == "true" %}
//! [`FirmwareUpdate`] реализован, когда при генерации выбрана OTA; образ
//! приходит командами `domain::ota::OtaCommand` от узла протокола.
// {%- endif %}
// {%- if settings_port == "true" %}
//! [`SettingsStorage`] — хранилище настроек.
// {%- if config == "true" %}
//! Его реализует `adapters::settings::Settings` над любым `NorFlash`, а строит
//! compose-site (`Settings::whole(board.config)`) над носителем, который плата
//! отдаёт полем `Board::config`:
// {%- if config_store == "eeprom" %}
//! внутренней EEPROM.
// {%- elsif config_store == "external" %}
//! разделом на внешней флеши.
// {%- else %}
//! разделом `CONFIG` внутреннего флеша.
// {%- endif %}
// {%- else %}
//! Раздела настроек у проекта нет: на его месте —
//! `adapters::settings::Unavailable`.
// {%- endif %}
// {%- endif %}
//! Порты протоколов связи и транспортов — только те, что выбраны при
//! генерации.
//!
// {%- if ota == "true" %}
//! Модули названы по операции домена, которая ими пользуется, а не по трейту:
//! в `ota::update` — порт, через который образ пишется и применяется, и записи,
//! которые обязаны одинаково понимать обе его стороны. Наружу всё
//! реэкспортируется в
//! корень — путь `ports::FirmwareUpdate` короче, а внутреннее деление
//! пользователя не касается (`docs/conventions.md`, «`lib.rs` только объявляет
//! модули»).
// {%- else %}
//! Наружу всё реэкспортируется в корень, а внутреннее деление пользователя не
//! касается (`docs/conventions.md`, «`lib.rs` только объявляет модули»).
// {%- endif %}

// {%- if has_can == "true" %}
// {%- if has_canopen == "true" %}
/// Шина CAN: кадры для узла CANopen.
// {%- else %}
/// Шина CAN: кадры для вашего протокола.
// {%- endif %}
pub mod can;

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
/// Modbus: предел PDU, коды исключений, транспорт master'а, ошибки обмена.
pub mod modbus;

// {%- endif %}
// {%- if ota == "true" %}
/// OTA: запись во флеш, запись о подписи для bootloader'а, отпечаток раскладки,
/// перезагрузка.
pub mod ota;

// {%- endif %}
// {%- if settings_port == "true" %}
/// Хранилище настроек.
pub mod settings;
// {%- endif %}

// {%- if has_can == "true" %}
pub use can::CanBus;
// {%- endif %}
// {%- if link_port == "true" %}
pub use link::{FrameDuplex, FrameLink, LinkError};
// {%- endif %}
// {%- if modbus_tcp == "true" %}
pub use link::HangUp;
// {%- endif %}
// {%- if net_services contains "mqtt" %}
pub use dial::Dial;
// {%- endif %}
// {%- if has_modbus == "true" %}
pub use modbus::{Exception, MAX_PDU};
// {%- if modbus_master == "true" %}
pub use modbus::{MasterError, ModbusClient};
// {%- endif %}
// {%- endif %}
// {%- if ota == "true" %}
pub use ota::reboot::Reboot;
pub use ota::update::{
    FirmwareUpdate, LAYOUT_SYMBOLS, MAX_WRITE_SIZE, PendingImage, layout_fingerprint,
};
// {%- endif %}
// {%- if settings_port == "true" %}
pub use settings::SettingsStorage;
// {%- endif %}
