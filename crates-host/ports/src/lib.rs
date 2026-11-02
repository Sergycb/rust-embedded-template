#![cfg_attr(not(test), no_std)]
// {%- if ports_async == "true" or net_services contains "ssh" %}
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

//! Порты — все трейты проекта и то, без чего их не выразить: типы из сигнатур
//! и форматы, которые обязаны совпадать у реализации и логики.
//!
//! Трейт здесь, только если одна логика работает с разными реализациями — в
//! разных узлах одной прошивки или в разных вариантах генерации; у трейта с
//! одной реализацией места нет, логика зовёт конкретный тип. Исключение —
//! рычаг платы, который зовёт логика узла: `domain` не видит `bsp`, и рычаг
//! приходит к ней трейтом даже при одной реализации.
//! Реализации живут в `adapters-logic` (каналы и транспорты протоколов над
//! потоком и сокетами), в `crates-cross/bsp` (то, чему нужна периферия
//! конкретного чипа) и в `domain` (карта регистров, ключ хоста) —
//! docs/architecture.md, «Трейт — только под несколько реализаций».
//!
//! Отсюда и то, чего здесь нет: сведения о чипе (`bsp::ChipSpec`) — данные, а
//! не поведение, и их тип живёт в `bsp`, рядом с `Board`. Причина сброса и
//! сообщение прошлой паники приходят в сведения об устройстве строками от `app`.
//!
//! Нет здесь и того, что нужно только логике, — команд узлам, ошибок её
//! операций: они в `domain`, рядом с узлом. Реализация порта их не видит, и
//! правка их в `ports` задевала бы `adapters-logic` и `bsp` зря.
//!
// {%- if ota == "true" %}
//! С OTA здесь контракт обновления с платой и bootloader'ом
//! ([`Partitions`], [`PendingImage`], [`layout_fingerprint`]) и перезагрузка
//! ([`Reboot`]); образ приходит командами `domain::ota::OtaCommand` от узла
//! протокола.
// {%- endif %}
//! Порты протоколов связи и транспортов — только те, что выбраны при
//! генерации.
//!
// {%- if ota == "true" %}
//! Модули названы по операции домена, которая ими пользуется, а не по трейту:
//! в `ota::update` — разделы, в которые образ пишется, и записи, которые
//! обязаны одинаково понимать плата, приложение и bootloader. Наружу всё
//! реэкспортируется в
//! корень — путь `ports::Partitions` короче, а внутреннее деление
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
// {%- if net_services contains "ssh" %}
/// Ключ хоста SSH: вшитый в прошивку или созданный платой.
pub mod host_key;

// {%- endif %}
// {%- if has_modbus == "true" %}
/// Modbus: предел PDU, коды исключений, данные slave'а, транспорт master'а,
/// ошибки обмена.
pub mod modbus;

// {%- endif %}
// {%- if ota == "true" %}
/// OTA: запись во флеш, запись о подписи для bootloader'а, отпечаток раскладки,
/// перезагрузка.
pub mod ota;

// {%- endif %}

// {%- if has_can == "true" %}
pub use can::CanBus;
// {%- endif %}
// {%- if link_port == "true" %}
pub use link::{FrameDuplex, FrameLink, LinkError};
// {%- endif %}
// {%- if net_services contains "ssh" %}
pub use host_key::HostKey;
// {%- endif %}
// {%- if has_modbus == "true" %}
pub use modbus::{Bits, BitsMut, Exception, MAX_PDU, Registers, Words, WordsMut};
// {%- if modbus_master == "true" %}
pub use modbus::{MasterError, ModbusClient};
// {%- endif %}
// {%- endif %}
// {%- if ota == "true" %}
pub use ota::reboot::Reboot;
pub use ota::update::{
    LAYOUT_SYMBOLS, MAX_WRITE_SIZE, Partitions, PendingImage, layout_fingerprint,
};
// {%- endif %}
