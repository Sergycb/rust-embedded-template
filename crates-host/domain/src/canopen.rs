//! Узел `CANOPEN`: узел CANopen (CiA 301) на шине CAN — стек
//! `zencan`.
//!
//! Стек ведёт себя так, как ждёт от него `zencan-node`: входящие кадры
//! кладутся в почтовый ящик (`NodeMbox::store_message`), периодически
//! вызывается `Node::process` — на нём heartbeat, таймауты SDO, PDO, — и всё,
//! что узел поставил в очередь передачи, уходит на шину. Узел делает
//! ровно это: ждёт кадр наперегонки с тактом
//! [`TICK`](crate::canopen::TICK){% if ota == "true" %} и с исходом команды
//! OTA, которую отправил, не дожидаясь (`canopen::ota`){% endif %}.
//!
//! Словарь объектов — проектный: его `zencan-build` генерирует из
//! `device.toml` крейта `crates-host/canopen`, а compose-site передаёт узлу
//! ссылки на сгенерированные `static`
//! ([`Dictionary`](crate::canopen::Dictionary)). Шина —
//! [`CanBus`](ports::CanBus) из `bsp`.
//!
//! # Сохранение объектов и адреса
//!
//! Мастер сохраняет настроенные по шине объекты записью `save` в `0x1010`, а
//! адрес, назначенный по LSS, — командой «Store configuration»; при старте
//! узел их восстанавливает. Как и где лежат — подмодуль `persisted`.
//!
//! # Почему `zencan`
//!
//! Кандидаты прогонялись одними и теми же проверками SDO-сервера (segmented
//! initiate на 4-байтовом объекте, abort от клиента, сегментированная запись
//! строки, неверный toggle, несуществующий объект) и фаззингом случайными
//! кадрами SDO:
//!
//! * `zencan` — все проверки, кроме одной: на abort клиента отвечает своим
//!   abort'ом вместо тишины (по CiA 301 ответа быть не должно, хосты это
//!   переносят); 0 паник. Словарь из TOML-описания, сохранение объектов,
//!   LSS, объекты загрузчика `0x5500` (прошивка по CAN). MPL-2.0 —
//!   исключение в `deny.toml`. Ещё два отступления нашлись уже здесь, и
//!   оба узел исправляет: кадр boot-up уходит с байтом 0x7F вместо 0x00
//!   (см. `step`), а сигнатуру `save` в `0x1010` стек ждёт с байтами в
//!   обратном порядке (см. `fix_save_signature`);
//! * `canopen-rs` (MIT/Apache) — все проверки, 0 паник, но строки и DOMAIN
//!   не длиннее 32 байт — прошивку по CAN через него не передать;
//! * `oxictl` — SDO-сервер записывает размер сегментированной передачи как
//!   значение и отвечает на abort, три параллельных словаря объектов;
//! * `funcan-rs` — заготовка без NMT/SDO/PDO; `oze-canopen`,
//!   `industrial-protocols-canopen` — требуют `std`.

mod frame;
// {%- if ota == "true" %}
mod ota;
// {%- endif %}
mod persisted;
mod stack;
mod wiring;

use core::cell::RefCell;
use core::convert::Infallible;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::Instant;
use embedded_can::Frame;
use ports::{CanBus, LinkError, SettingsStorage};
use supervisor::runtime::TaskExit;
use zencan_node::common::NodeId;
use zencan_node::common::messages::CanMessage;
use zencan_node::common::nmt::NmtState;
use zencan_node::embedded_io::Read;
use zencan_node::object_dict::ODEntry;
use zencan_node::{Callbacks, Node};

use crate::canopen::frame::{fix_save_signature, is_heartbeat, to_frame, to_message};

pub use persisted::{NODE_ID_KEY, OBJECTS_KEY, Persisted, STORE_CAPACITY};
pub use stack::{Stack, TICK};
pub use wiring::{CanOpenWiring, Dictionary, dictionary, serial};
// {%- if ota == "true" %}
pub use ota::{OTA, OTA_INDEX, OtaObject};
// {%- endif %}

// Узел объявлен рядом со своей задачей.
//
// `<C>` и `<S>` — параметры фрагмента: шину и хранилище настроек платы
// `domain` не видит. `local` — шина и хранилище вправе быть `!Send`
// (периферия). Словарь — обычный слот: ссылки на `static` — `Send`. Стек и
// его колбэки слотов не занимают: они в кадре задачи (`task`).
supervisor::supervisor_fragment! {
    name: CANOPEN_FRAG<C, S>;
    boot: inputs: $crate::canopen::CanOpenWiring<C, S>;

    node deps: [], restart: Never,
        resources: [
            /// Шина CAN платы.
            BUS: local C = inputs.bus,
            /// Адрес узла на шине, 1..=127.
            NODE_ID: u8 = inputs.node_id,
            /// Словарь объектов устройства.
            DICTIONARY: $crate::canopen::Dictionary = inputs.dictionary,
            /// Хранилище сохранённых объектов и адреса узла — собирает его
            /// узел над рычагом платы.
            SETTINGS: local S = <S as $crate::Assemble>::assemble(inputs.settings, ()),
            /// Сохранять ли объекты и адрес: `false` — хранилища у проекта нет.
            PERSIST: bool = inputs.persist,
            /// Серийный номер объекта идентичности (0x1018:04).
            SERIAL: u32 = inputs.serial,
            // {%- if ota == "true" %}
            /// Номер клиента узла OTA — транспорта.
            OTA_CLIENT: $crate::ota::ClientId = inputs.ota_client
            // {%- endif %}
        ],
        // {%- if ota == "true" %}
        // Команды объекта OTA словаря ([`OTA`]) — на вход `ASK` узла OTA без
        // ожидания исхода: узел обслуживает шину, пока OTA стирает раздел.
        // Квота — одна команда: следующую объект не ставит до исхода.
        asks: [OTA.ASK: $crate::ota::OtaRequest -> $crate::ota::OtaReply; 1 tag ()],
        // {%- endif %}
        ctx: $crate::canopen::CanOpenCtx,
        task: $crate::canopen::task;
}

#[fsm::typestate]
impl<'f, C, S> CanOpenCtx<'_, C, S>
where
    C: CanBus,
    C::Error: FormatOrDebug,
    S: SettingsStorage,
    S::Error: FormatOrDebug,
{
    fsm::machine! {
        machine CanOpen mod edges {
            states {
                /// Стек поднят, первая обработка ещё не прошла.
                Booting { stack: Stack<'f> },
                /// Стек работает; `next` — ближайший такт.
                Running { stack: Stack<'f>, next: Instant },
            }
            start(
                persisted: &'f RefCell<Persisted>,
                callbacks: &'f mut Option<Callbacks<'f>>,
                node: &'f mut Option<Node<'f>>,
            ) => begin -> Booting;
            listen {
                // Такт — первым: под потоком кадров heartbeat не должен
                // откладываться.
                Running { next, .. } => tick: fsm::deadline(*next),
                // {%- if ota == "true" %}
                // Исход команды OTA — до кадров: держит квоту, пока не забран.
                Running             => ota: self.ota_ask.reply(),
                // {%- endif %}
                Running             => frame: self.bus.recv(),
            }
            transitions {
                Booting                    => boot     -> Running,
                Running + tick(())         => tick     -> self,
                Running + frame(Ok(frame)) => received -> self,
                Running + frame(Err(err))  => failed   -> self,
                // {%- if ota == "true" %}
                Running + ota(reply)       => answered -> self,
                // {%- endif %}
            }
        }
    }

    /// Поднимает стек: адрес и сохранённые объекты — из хранилища, затем
    /// `Node::new`.
    async fn begin(
        &mut self,
        persisted: &'f RefCell<Persisted>,
        callbacks: &'f mut Option<Callbacks<'f>>,
        node: &'f mut Option<Node<'f>>,
    ) -> Booting<'f> {
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
        info!("domain: узел CANOPEN запущен, адрес {}", node_id.raw());
        // `Node::new` сам зовёт `reset_app` — тот восстанавливает объекты из
        // прочитанного `restore`.
        let node = node.insert(Node::new(
            node_id,
            callbacks.take().unwrap_or_default(),
            d.mbox,
            d.state,
            d.od,
        ));
        Booting {
            stack: Stack {
                node,
                start: Instant::now(),
                persisted,
            },
        }
    }

    /// Первая обработка: стек ставит в очередь кадр boot-up, он уходит на
    /// шину.
    async fn boot(&mut self, s: Booting<'f>) -> Running<'f> {
        let mut running = Running {
            stack: s.stack,
            next: Instant::now() + TICK,
        };
        self.step(&mut running).await;
        running
    }

    /// Такт: обработка без нового кадра.
    async fn tick(&mut self, s: &mut Running<'f>) {
        s.next = fsm::next_tick(s.next, TICK, Instant::now());
        self.step(s).await;
    }

    /// Кадр с шины — в почтовый ящик, затем обработка.
    ///
    /// Кадр, не адресованный узлу (чужой heartbeat, PDO, SDO других узлов),
    /// почтовый ящик не принимает — он пропускается без обработки: на
    /// загруженной шине таких большинство.
    async fn received(&mut self, s: &mut Running<'f>, frame: C::Frame) {
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

    /// Кадр потерян или шина отказала — в лог, и ждём следующий кадр.
    #[expect(clippy::unused_self, reason = "действие автомата")]
    fn failed(&mut self, err: LinkError<C::Error>) {
        match err {
            LinkError::Lost(err) => warn!("canopen: кадр потерян: {:?}", &err),
            LinkError::Down(err) => warn!("canopen: шина недоступна: {:?}", &err),
        }
    }

    // {%- if ota == "true" %}
    /// Исход команды OTA — в объект словаря (sub 5 и 6).
    #[expect(clippy::unused_self, reason = "действие автомата")]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "событие `listen` автомат отдаёт действию по значению"
    )]
    fn answered(&mut self, reply: supervisor::AskReply<(), crate::ota::OtaReply>) {
        let reply = reply.outcome.unwrap_or_else(|_dropped| {
            warn!("canopen: узел OTA не ответил");
            Err(ports::Rejection::Device)
        });
        OTA.record(reply);
    }
    // {%- endif %}

    /// `Node::process`, затем всё, что стек поставил в очередь, — на шину.
    async fn step(&mut self, s: &mut Running<'f>) {
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
        persisted::save(s.stack.persisted, self.settings).await;
        // {%- if ota == "true" %}
        // Команда OTA, поставленная записью SDO, — на вход `ASK` узла OTA, не
        // дожидаясь исхода: его принесёт `reply()` (`answered`).
        OTA.send(&mut self.ota_ask, *self.ota_client);
        // {%- endif %}
    }

    /// Адрес узла и сохранённые объекты — из хранилища, до создания стека.
    /// Без хранилища (`persist` — `false`) не читает ничего.
    ///
    /// # Panics
    ///
    /// Если `node_id` во входе не 1..=127, а сохранённого адреса нет.
    async fn restore(&mut self, persisted: &RefCell<Persisted>) -> NodeId {
        if *self.persist
            && let Some(node_id) = persisted::load(persisted, self.settings).await
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

/// Узел `CANOPEN`: ведёт стек CANopen, пока жива шина.
///
/// Не выходит никогда: отказ шины — в лог, и узел снова ждёт кадр тем же
/// стеком. Переждать bus-off — дело реализации [`CanBus`].
///
/// Буфер сохранённых объектов и колбэки стека над ним — локальные этой
/// функции: стек держит колбэки ссылками на всё время работы, и жить им,
/// кроме кадра задачи, негде — тип замыкания не назвать в `static`, а поле
/// контекста на время автомата не заимствовать. Автомат получает их
/// аргументами стартового действия (`start(..)` в таблице). Без хранилища
/// (`persist` во входе — `false`) колбэков нет: стек отвечает на `save` и на
/// LSS «Store configuration» отказом.
///
/// Без `watchdog:` намеренно:
/// узлу нечем отметиться, кроме собственного такта, а он сторожил бы
/// исполнитель, а не работу.
///
/// # Panics
///
/// Если `node_id` во входе не 1..=127, а сохранённого адреса нет, — ошибка
/// конфигурации.
pub async fn task<C, S>(mut ctx: CanOpenCtx<'_, C, S>) -> TaskExit
where
    C: CanBus,
    C::Error: FormatOrDebug,
    S: SettingsStorage,
    S::Error: FormatOrDebug,
{
    let persisted = RefCell::new(Persisted::new());
    let mut store_objects = |reader: &mut dyn Read<Error = Infallible>, size: usize| {
        persisted.borrow_mut().store(reader, size);
    };
    let mut store_node_config = |node_id: NodeId| {
        persisted.borrow_mut().store_node_id(node_id);
    };
    let mut reset_app = |od: &[ODEntry<'_>]| persisted.borrow().restore(od, false);
    let mut reset_comms = |od: &[ODEntry<'_>]| persisted.borrow().restore(od, true);
    let mut node = None;
    let mut callbacks = (*ctx.persist).then(|| Callbacks {
        store_objects: Some(&mut store_objects),
        store_node_config: Some(&mut store_node_config),
        reset_app: Some(&mut reset_app),
        reset_comms: Some(&mut reset_comms),
        ..Callbacks::default()
    });
    match ctx.run(&persisted, &mut callbacks, &mut node).await {}
}

#[cfg(test)]
mod tests;
