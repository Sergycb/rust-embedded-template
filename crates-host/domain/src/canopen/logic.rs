//! Логика узла CANopen: стек `zencan`, сохранённые объекты и команды OTA.
//!
//! Задача [`task`](super::task) собирает [`CanOpen`] из полей контекста и
//! крутит его цикл; здесь — что узел делает с кадром, тактом и исходом OTA.

use core::cell::RefCell;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::Instant;
use embedded_can::Frame;
use embedded_storage_async::nor_flash::NorFlash;
use ports::{CanBus, LinkError};
use zencan_node::common::NodeId;
use zencan_node::common::messages::CanMessage;
use zencan_node::common::nmt::NmtState;
use zencan_node::{Callbacks, Node};

// {%- if ota == "true" %}
use super::ota::OTA;
use crate::ota::{ClientId, OtaReply, OtaRequest};
// {%- endif %}
use super::frame::{fix_save_signature, is_heartbeat, to_frame, to_message};
use super::persisted;
use super::persisted::Persisted;
use super::stack::{Stack, TICK};
use super::wiring::Dictionary;
use crate::settings::{Error as SettingsError, Settings};
use crate::tick::next_tick;

/// Стек в работе и ближайший такт: состояние, которое переживает круг цикла.
pub(super) struct Running<'f> {
    pub(super) stack: Stack<'f>,
    /// Ближайший такт.
    pub(super) next: Instant,
}

/// Узел CANopen: шина, словарь, хранилище и вход `ASK` узла OTA.
pub(super) struct CanOpen<'a, C, S> {
    /// Шина CAN платы.
    pub(super) bus: &'a mut C,
    /// Адрес узла на шине, 1..=127.
    node_id: &'a mut u8,
    /// Словарь объектов устройства.
    dictionary: &'a mut Dictionary,
    /// Хранилище сохранённых объектов и адреса узла; `None` — раздела
    /// настроек у проекта нет, и узел ничего не сохраняет.
    settings: &'a mut Option<S>,
    /// Серийный номер объекта идентичности (0x1018:04).
    serial: &'a mut u32,
    // {%- if ota == "true" %}
    /// Номер клиента узла OTA — транспорта.
    ota_client: &'a mut ClientId,
    /// Вход `ASK` узла OTA: команды без ожидания исхода.
    pub(super) ota_ask: supervisor::Asker<'a, OtaRequest, OtaReply, ()>,
    // {%- endif %}
}

impl<'a, C, S> CanOpen<'a, C, S> {
    /// Собирает логику узла из полей контекста.
    #[must_use]
    pub(super) fn new(
        bus: &'a mut C,
        node_id: &'a mut u8,
        dictionary: &'a mut Dictionary,
        settings: &'a mut Option<S>,
        serial: &'a mut u32,
        // {%- if ota == "true" %}
        ota_client: &'a mut ClientId,
        ota_ask: supervisor::Asker<'a, OtaRequest, OtaReply, ()>,
        // {%- endif %}
    ) -> Self {
        Self {
            bus,
            node_id,
            dictionary,
            settings,
            serial,
            // {%- if ota == "true" %}
            ota_client,
            ota_ask,
            // {%- endif %}
        }
    }
}

impl<C, F> CanOpen<'_, C, Settings<F>>
where
    C: CanBus,
    C::Error: FormatOrDebug,
    F: NorFlash,
    SettingsError<F::Error>: FormatOrDebug,
{
    /// Поднимает стек: адрес и сохранённые объекты — из хранилища, затем
    /// `Node::new` и первая обработка — стек ставит в очередь кадр boot-up, и
    /// он уходит на шину.
    pub(super) async fn begin<'f>(
        &mut self,
        persisted: &'f RefCell<Persisted>,
        callbacks: &'f mut Option<Callbacks<'f>>,
        node: &'f mut Option<Node<'f>>,
    ) -> Running<'f> {
        let d = *self.dictionary;
        // {%- if ota == "true" %}
        // Объект OTA в словаре — место под обработчик: без регистрации стек
        // отвечает на его SDO отказом `ResourceNotAvailable`.
        d.ota.register_handler(&OTA);
        OTA.reset();
        // {%- endif %}
        // Серийный номер — до `Node::new`: стек и LSS читают идентичность из
        // словаря уже при старте.
        d.identity.set_serial(*self.serial);
        let node_id = self.restore(persisted).await;
        info!("canopen: узел запущен, адрес {}", node_id.raw());
        // `Node::new` сам зовёт `reset_app` — тот восстанавливает объекты из
        // прочитанного `restore`.
        let node = node.insert(Node::new(
            node_id,
            callbacks.take().unwrap_or_default(),
            d.mbox,
            d.state,
            d.od,
        ));
        let mut running = Running {
            stack: Stack {
                node,
                start: Instant::now(),
                persisted,
            },
            next: Instant::now() + TICK,
        };
        self.step(&mut running).await;
        running
    }

    /// Такт: обработка без нового кадра.
    pub(super) async fn tick(&mut self, s: &mut Running<'_>) {
        s.next = next_tick(s.next, TICK, Instant::now());
        self.step(s).await;
    }

    /// Кадр с шины — в почтовый ящик, затем обработка.
    ///
    /// Кадр, не адресованный узлу (чужой heartbeat, PDO, SDO других узлов),
    /// почтовый ящик не принимает — он пропускается без обработки: на
    /// загруженной шине таких большинство.
    pub(super) async fn received(&mut self, s: &mut Running<'_>, frame: C::Frame) {
        let Some(mut message) = to_message(&frame) else {
            warn!(
                "canopen: кадр CAN FD на {} байт пропущен — стек классический",
                frame.data().len()
            );
            return;
        };
        fix_save_signature(&mut message);
        if self.dictionary.mbox.store_message(message).is_ok() {
            self.step(s).await;
        }
    }

    // {%- if ota == "true" %}
    /// Исход команды OTA — в объект словаря (sub 5 и 6).
    #[expect(clippy::unused_self, reason = "действие автомата")]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "событие `listen` автомат отдаёт действию по значению"
    )]
    pub(super) fn answered(&mut self, reply: supervisor::AskReply<(), OtaReply>) {
        let reply = reply.outcome.unwrap_or_else(|_dropped| {
            warn!("canopen: узел OTA не ответил");
            Err(crate::ota::command::Rejection::Device)
        });
        OTA.record(reply);
    }
    // {%- endif %}

    /// `Node::process`, затем всё, что стек поставил в очередь, — на шину.
    async fn step(&mut self, s: &mut Running<'_>) {
        let node = &mut *s.stack.node;
        // Стек объявляет о себе кадром boot-up при первом запуске, после
        // NMT Reset Node / Reset Communication и после смены адреса — в
        // обработке, которая начинается из состояния `Bootup`.
        let booting = node.nmt_state() == NmtState::Bootup;
        let id_before = node.node_id();
        // Время — от старта стека, а не от включения МК: `Node` отсчитывает
        // его с нуля, и абсолютное время дало бы на первой обработке
        // огромный интервал (лишний heartbeat, обрезка до `u32`).
        node.process((Instant::now() - s.stack.start).as_micros());
        let node_id = node.node_id();
        let mut boot_up = booting || node_id != id_before;
        while let Some(mut message) = self.dictionary.mbox.next_transmit_message() {
            let announcing = boot_up && is_heartbeat(&message, node_id);
            if announcing {
                // CiA 301: кадр boot-up — `0x700 + адрес` с байтом 0x00.
                // `zencan-node` 0.0.5 переводит узел в pre-operational до
                // отправки и шлёт 0x7F — обычный heartbeat (коммит d90a383
                // «General updates», без обоснования; до него слал 0x00), и
                // NMT-master, ждущий boot-up, не узнал бы о перезапуске узла.
                // Убрать, когда исправят в zencan.
                message.data[0] = 0x00;
                boot_up = false;
            }
            self.transmit(&message).await;
        }
        // После ответов: запись во флеш долгая, а мастер ждёт подтверждения
        // `save` по SDO. Хранилищу шина не нужна: `save` уже принят стеком.
        if let Some(settings) = self.settings.as_mut() {
            persisted::save(s.stack.persisted, settings).await;
        }
        // {%- if ota == "true" %}
        // Команда OTA, поставленная записью SDO, — на вход `ASK` узла OTA, не
        // дожидаясь исхода: его принесёт `reply()` (`answered`).
        OTA.send(&mut self.ota_ask, *self.ota_client);
        // {%- endif %}
    }

    /// Адрес узла и сохранённые объекты — из хранилища, до создания стека.
    /// Без хранилища (`None`) не читает ничего.
    ///
    /// # Panics
    ///
    /// Если `node_id` во входе не 1..=127, а сохранённого адреса нет.
    async fn restore(&mut self, persisted: &RefCell<Persisted>) -> NodeId {
        if let Some(settings) = self.settings.as_mut()
            && let Some(node_id) = persisted::load(persisted, settings).await
        {
            return node_id;
        }
        match NodeId::new(*self.node_id) {
            Ok(node_id @ NodeId::Configured(_)) => node_id,
            _ => panic!("адрес узла CANopen {} вне 1..=127", *self.node_id),
        }
    }

    /// Сообщение стека — на шину; отказ (кадр не принят шиной, потеря, bus-off)
    /// — в лог: кадр пропадает, а мастер уходит по таймауту.
    async fn transmit(&mut self, message: &CanMessage) {
        let Some(frame) = to_frame::<C::Frame>(message) else {
            warn!("canopen: стек поставил в очередь кадр, который шина не примет");
            return;
        };
        match self.bus.send(&frame).await {
            Ok(()) => {}
            Err(LinkError::Lost(err)) => warn!("canopen: кадр не отправлен: {:?}", &err),
            Err(LinkError::Down(err)) => warn!("canopen: шина недоступна: {:?}", &err),
        }
    }
}
