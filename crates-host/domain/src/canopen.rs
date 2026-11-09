//! Узел `CANOPEN_<транспорт>`: узел CANopen (CiA 301) на шине CAN — стек
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
//!   исключение в `deny.toml`. Ещё четыре отступления нашлись уже здесь. Два
//!   узел исправляет: кадр boot-up уходит с байтом 0x7F вместо 0x00 (см.
//!   `step`), а сигнатуру `save` в `0x1010` стек ждёт с байтами в обратном
//!   порядке (см. `fix_save_signature`). Два остаются до релиза `zencan` с
//!   исправлением (оно уже принято upstream): NMT Reset Node не возвращает
//!   несохранённые объекты приложения к умолчаниям (сохранённые узел
//!   восстанавливает сам), а адрес из LSS «Configure node-ID» стек применяет на
//!   следующей обработке, а не после NMT Reset Communication (CiA 305);
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
// {%- if ota == "true" %}
// Гонка такта, исхода OTA и кадра: `select3`.
use embassy_futures::select::{Either3, select3};
// {%- else %}
// Гонка такта и кадра: `select`.
use embassy_futures::select::{Either, select};
// {%- endif %}
use embassy_futures::yield_now;
use embassy_time::Instant;
use embedded_can::Frame;
use embedded_storage_async::nor_flash::NorFlash;
use ports::{CanBus, LinkError};
use zencan_node::common::NodeId;
use zencan_node::common::messages::CanMessage;
use zencan_node::common::nmt::NmtState;
use zencan_node::embedded_io::Read;
use zencan_node::object_dict::ODEntry;
use zencan_node::{Callbacks, Node};

use crate::canopen::frame::{fix_save_signature, is_heartbeat, to_frame, to_message};
use crate::settings::{Error as SettingsError, Settings};
use crate::tick::{deadline, next_tick};

pub use persisted::{NODE_ID_KEY, OBJECTS_KEY, Persisted, STORE_CAPACITY};
pub use stack::{Stack, TICK};
pub use wiring::{Dictionary, dictionary, serial};
// {%- if ota == "true" %}
pub use ota::{OTA, OTA_INDEX, OtaObject};
// {%- endif %}

// Узел объявлен рядом со своей задачей.
//
// `<C>` и `<S>` — параметры фрагмента: шину и хранилище настроек над
// носителем платы (`settings::Settings<…>`) `domain` не видит. `local` — шина и хранилище вправе быть `!Send`
// (периферия). Словарь — обычный слот: ссылки на `static` — `Send`. Стек и
// его колбэки слотов не занимают: они в кадре задачи (`task`).
supervisor::supervisor_fragment! {
    name: CANOPEN_FRAG<C, S>;
    boot: inputs: struct $crate::canopen::CanOpenWiring;

    node resources: [
            /// Шина CAN платы.
            BUS: local C = inputs.bus,
            /// Адрес узла на шине, 1..=127.
            NODE_ID: u8 = inputs.node_id,
            /// Словарь объектов устройства.
            DICTIONARY: $crate::canopen::Dictionary = inputs.dictionary,
            /// Хранилище сохранённых объектов и адреса узла; `None` — раздела
            /// настроек у проекта нет, и узел ничего не сохраняет.
            SETTINGS: local Option<S> = inputs.settings,
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

/// Стек в работе и ближайший такт: состояние, которое переживает круг цикла.
struct Running<'f> {
    stack: Stack<'f>,
    /// Ближайший такт.
    next: Instant,
}

impl<C, F> CanOpenCtx<'_, C, Settings<F>>
where
    C: CanBus,
    C::Error: FormatOrDebug,
    F: NorFlash,
    SettingsError<F::Error>: FormatOrDebug,
{
    /// Поднимает стек: адрес и сохранённые объекты — из хранилища, затем
    /// `Node::new` и первая обработка — стек ставит в очередь кадр boot-up, и
    /// он уходит на шину.
    async fn begin<'f>(
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
    async fn tick<'f>(&mut self, s: &mut Running<'f>) {
        s.next = next_tick(s.next, TICK, Instant::now());
        self.step(s).await;
    }

    /// Кадр с шины — в почтовый ящик, затем обработка.
    ///
    /// Кадр, не адресованный узлу (чужой heartbeat, PDO, SDO других узлов),
    /// почтовый ящик не принимает — он пропускается без обработки: на
    /// загруженной шине таких большинство.
    async fn received<'f>(&mut self, s: &mut Running<'f>, frame: C::Frame) {
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
    fn answered(&mut self, reply: supervisor::AskReply<(), crate::ota::OtaReply>) {
        let reply = reply.outcome.unwrap_or_else(|_dropped| {
            warn!("canopen: узел OTA не ответил");
            Err(crate::ota::command::Rejection::Device)
        });
        OTA.record(reply);
    }
    // {%- endif %}

    /// `Node::process`, затем всё, что стек поставил в очередь, — на шину.
    async fn step<'f>(&mut self, s: &mut Running<'f>) {
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

/// Узел `CANOPEN`: ведёт стек CANopen, пока жива шина.
///
/// Не выходит никогда: отказ шины — в лог, и узел снова ждёт кадр тем же
/// стеком. Переждать bus-off — дело реализации [`CanBus`].
///
/// Буфер сохранённых объектов и колбэки стека над ним — локальные этой
/// функции: стек держит колбэки ссылками на всё время работы, и жить им,
/// кроме кадра задачи, негде — тип замыкания не назвать в `static`, а поле
/// контекста на всё время работы не заимствовать. Поэтому `begin` получает
/// их ссылками. Без хранилища
/// (`settings` во входе — `None`) колбэков нет: стек отвечает на `save` и на
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
pub async fn task<C, F>(mut ctx: CanOpenCtx<'_, C, Settings<F>>)
where
    C: CanBus,
    C::Error: FormatOrDebug,
    F: NorFlash,
    SettingsError<F::Error>: FormatOrDebug,
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
    let mut callbacks = ctx.settings.is_some().then(|| Callbacks {
        store_objects: Some(&mut store_objects),
        store_node_config: Some(&mut store_node_config),
        reset_app: Some(&mut reset_app),
        reset_comms: Some(&mut reset_comms),
        ..Callbacks::default()
    });
    let mut running = ctx.begin(&persisted, &mut callbacks, &mut node).await;
    loop {
        // Такт — первым: под потоком кадров heartbeat не должен
        // откладываться. Срок, наступивший до начала круга, обрабатывается
        // без опроса шины{% if ota == "true" %} и исхода OTA{% endif %}: как
        // в таблице `listen`, источник не опрашивается.
        if running.next <= Instant::now() {
            ctx.tick(&mut running).await;
            // Такту по наступившему сроку — круг исполнителю: иначе
            // накопившаяся очередь тактов крутила бы узел, не отдавая
            // другим задачам.
            yield_now().await;
            continue;
        }
        // Источники — в порядке приоритета: такт{% if ota == "true" %}, исход
        // OTA{% endif %}, кадр.
        // {%- if ota == "true" %}
        match select3(deadline(running.next), ctx.ota_ask.reply(), ctx.bus.recv()).await {
            Either3::First(()) => {
                ctx.tick(&mut running).await;
                yield_now().await;
            }
            Either3::Second(reply) => ctx.answered(reply),
            Either3::Third(result) => match result {
                Ok(frame) => ctx.received(&mut running, frame).await,
                // Кадр потерян или шина отказала — в лог, и ждём следующий
                // кадр.
                Err(LinkError::Lost(err)) => warn!("canopen: кадр потерян: {:?}", &err),
                Err(LinkError::Down(err)) => warn!("canopen: шина недоступна: {:?}", &err),
            },
        }
        // {%- else %}
        match select(deadline(running.next), ctx.bus.recv()).await {
            Either::First(()) => {
                ctx.tick(&mut running).await;
                yield_now().await;
            }
            Either::Second(result) => match result {
                Ok(frame) => ctx.received(&mut running, frame).await,
                // Кадр потерян или шина отказала — в лог, и ждём следующий
                // кадр.
                Err(LinkError::Lost(err)) => warn!("canopen: кадр потерян: {:?}", &err),
                Err(LinkError::Down(err)) => warn!("canopen: шина недоступна: {:?}", &err),
            },
        }
        // {%- endif %}
    }
}

#[cfg(test)]
mod tests {
    //! Тесты узла `CANOPEN`: гоняют узел на словаре примера `crates-host/canopen`.

    use core::cell::RefCell;
    use core::future::poll_fn;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::sync::{Mutex, PoisonError};

    use adapters_hw::flash::fake::MemFlash;
    use embedded_can::{Frame, Id, StandardId};
    use ports::{CanBus, LinkError};

    use super::{NODE_ID_KEY, task};
    use crate::settings::Settings;

    /// Адрес узла в тестах: SDO-запросы — на `0x605`, ответы — с `0x585`.
    const NODE: u8 = 5;

    /// Серийный номер узла в тестах — то, что отдаёт `0x1018:04`.
    const SERIAL: u32 = 0x1234_5678;

    /// SDO-запись `save` (`0x73 0x61 0x76 0x65`) в `0x1010:01`.
    const SAVE: [u8; 8] = [0x23, 0x10, 0x10, 0x01, 0x73, 0x61, 0x76, 0x65];

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestFrame {
        id: Id,
        data: Vec<u8>,
        remote: bool,
    }

    impl Frame for TestFrame {
        fn new(id: impl Into<Id>, data: &[u8]) -> Option<Self> {
            (data.len() <= 8).then(|| Self {
                id: id.into(),
                data: data.to_vec(),
                remote: false,
            })
        }

        fn new_remote(id: impl Into<Id>, dlc: usize) -> Option<Self> {
            (dlc <= 8).then(|| Self {
                id: id.into(),
                data: vec![0; dlc],
                remote: true,
            })
        }

        fn is_extended(&self) -> bool {
            matches!(self.id, Id::Extended(_))
        }

        fn is_remote_frame(&self) -> bool {
            self.remote
        }

        fn id(&self) -> Id {
            self.id
        }

        fn dlc(&self) -> usize {
            self.data.len()
        }

        fn data(&self) -> &[u8] {
            &self.data
        }
    }

    /// Входящие исходы `recv` шины — общие с тестом, который досылает кадры,
    /// пока узел работает.
    type Incoming = Rc<RefCell<VecDeque<Result<TestFrame, LinkError<&'static str>>>>>;

    /// Шина: отдаёт заготовленные исходы `recv`, когда они кончаются, — ждёт,
    /// как тихая шина; отправленные кадры складывает в `sent`.
    struct Bus {
        incoming: Incoming,
        sent: Vec<TestFrame>,
        send: Result<(), LinkError<&'static str>>,
    }

    impl Bus {
        fn with(
            incoming: impl IntoIterator<Item = Result<TestFrame, LinkError<&'static str>>>,
        ) -> Self {
            Self {
                incoming: Rc::new(RefCell::new(incoming.into_iter().collect())),
                sent: Vec::new(),
                send: Ok(()),
            }
        }
    }

    impl CanBus for Bus {
        type Frame = TestFrame;
        type Error = &'static str;

        fn recv(&mut self) -> impl Future<Output = Result<TestFrame, LinkError<Self::Error>>> {
            // Кадр снимается в том опросе, который его возвращает: брошенный
            // `recv` кадр не теряет — контракт порта.
            poll_fn(|_| {
                self.incoming
                    .borrow_mut()
                    .pop_front()
                    .map_or(Poll::Pending, Poll::Ready)
            })
        }

        fn send(
            &mut self,
            frame: &TestFrame,
        ) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            self.sent.push(frame.clone());
            std::future::ready(self.send)
        }
    }

    /// Носитель настроек в тестах: две страницы по 256 байт, слово 8.
    type Flash = MemFlash<512, 8, 256>;

    /// Хранилище узла в тестах: настоящее, над флешем в памяти; `None` —
    /// хранилища нет.
    type Store = Option<Settings<Flash>>;

    /// Словарь из примера — `static` на весь процесс: тесты, которые гоняют
    /// по нему узел, обязаны идти по одному, иначе один тест читает кадры
    /// другого (под `cargo test` они идут потоками одного процесса).
    static DICTIONARY: Mutex<()> = Mutex::new(());

    /// Пустое хранилище над стёртым флешем.
    fn memory() -> Settings<Flash> {
        Settings::new(Flash::erased(), 0..512)
    }

    /// Что записано под ключом `key`. Флеш в памяти не ждёт, поэтому чтение
    /// готово с первого опроса.
    fn stored(settings: &mut Store, key: u32) -> Option<Vec<u8>> {
        let settings = settings.as_mut().expect("хранилище есть");
        let mut scratch = [0; 128];
        let mut read = pin!(settings.read(key, &mut scratch));
        let Poll::Ready(read) = read.as_mut().poll(&mut Context::from_waker(Waker::noop())) else {
            panic!("чтение флеша в памяти не ждёт");
        };
        read.expect("чтение").map(<[u8]>::to_vec)
    }

    fn frame(id: u16, data: &[u8]) -> TestFrame {
        TestFrame::new(StandardId::new(id).unwrap(), data).unwrap()
    }

    /// Гоняет узел на словаре из примера `crates-host/canopen`, пока кадры не
    /// кончатся: узел не выходит, а ждёт следующий.
    fn serve(bus: &mut Bus) {
        serve_with(bus, &mut Some(memory()));
    }

    /// То же над заданным хранилищем (`None` — хранилища нет): новый прогон —
    /// новый стек над ним, как после сброса МК.
    fn serve_with(bus: &mut Bus, settings: &mut Store) {
        let _only_one = DICTIONARY.lock().unwrap_or_else(PoisonError::into_inner);
        // Почтовый ящик — `static`: очередь передачи после прошлого прогона к
        // этому не относится.
        while canopen::NODE_MBOX.next_transmit_message().is_some() {}
        let mut dictionary = super::dictionary();
        let mut node_id = NODE;
        let mut serial = SERIAL;
        // {%- if ota == "true" %}
        let mut ota_client = crate::ota::ClientId(0);
        // {%- endif %}
        let stand = supervisor::stand!(CANOPEN_FRAG<Bus, Settings<Flash>>);
        let ctx = stand.ctx(
            bus,
            &mut node_id,
            &mut dictionary,
            settings,
            &mut serial,
            // {%- if ota == "true" %}
            &mut ota_client,
            // {%- endif %}
        );
        let mut task = pin!(task(ctx));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт кадр");
    }

    // {%- if ota == "true" %}
    /// Команда OTA записью SDO уходит узлу OTA без ожидания исхода: пока
    /// узел OTA не ответил, узел CANopen обрабатывает кадры, и sub 5 читается
    /// нулём; исход приходит из `reply()` и читается следующим чтением.
    #[test]
    fn an_ota_command_over_sdo_does_not_stop_the_node() {
        /// Чтение исхода — 0x4F00:05.
        const READ_STATUS: [u8; 8] = [0x40, 0x00, 0x4F, 0x05, 0, 0, 0, 0];

        let _only_one = DICTIONARY.lock().unwrap_or_else(PoisonError::into_inner);
        while canopen::NODE_MBOX.next_transmit_message().is_some() {}
        let mut bus = Bus::with([
            // `Finish` — 0x4F00:03, expedited на 4 байта (cs 0x23).
            Ok(frame(0x605, &[0x23, 0x00, 0x4F, 0x03, 0, 0, 0, 0])),
            Ok(frame(0x605, &READ_STATUS)),
        ]);
        let incoming = Rc::clone(&bus.incoming);
        {
            let mut dictionary = super::dictionary();
            let (mut node_id, mut settings): (u8, Store) = (NODE, None);
            let mut serial = SERIAL;
            let mut ota_client = crate::ota::ClientId(0);
            let stand = supervisor::stand!(CANOPEN_FRAG<Bus, Settings<Flash>>);
            let mut ota_node =
                crate::ota::tests::serving(stand.ota_ask.server().expect("сервер ASK свободен"));
            let ctx = stand.ctx(
                &mut bus,
                &mut node_id,
                &mut dictionary,
                &mut settings,
                &mut serial,
                &mut ota_client,
            );
            let mut node = pin!(task(ctx));
            let mut cx = Context::from_waker(Waker::noop());
            let mut poll_node = || {
                assert!(
                    node.as_mut().poll(&mut cx).is_pending(),
                    "узел CANopen не выходит"
                );
            };
            // Узел OTA ещё не опрошен — исхода нет, а кадры обрабатываются.
            (0..8).for_each(|_| poll_node());
            // Узел OTA отвечает, узел CANopen забирает исход.
            for _ in 0..8 {
                poll_node();
                assert!(
                    ota_node
                        .as_mut()
                        .poll(&mut Context::from_waker(Waker::noop()))
                        .is_pending(),
                    "узел OTA не выходит"
                );
            }
            incoming
                .borrow_mut()
                .push_back(Ok(frame(0x605, &READ_STATUS)));
            (0..8).for_each(|_| poll_node());
        }

        let sdo: Vec<_> = bus
            .sent
            .iter()
            .filter(|f| f.id == Id::Standard(StandardId::new(0x585).unwrap()))
            .collect();
        assert_eq!(sdo[0].data[0], 0x60, "запись принята: {:?}", sdo[0]);
        // Ответы на чтение — expedited на 2 байта (cs 0x4B): сначала 0 —
        // выполняется, затем `NoSession` = 16.
        assert_eq!(sdo[1].data, [0x4B, 0x00, 0x4F, 0x05, 0, 0, 0, 0]);
        assert_eq!(sdo[2].data, [0x4B, 0x00, 0x4F, 0x05, 16, 0, 0, 0]);
    }
    /// Объект OTA в `device.toml` стоит по индексу [`super::OTA_INDEX`]:
    /// разойдись они — хост писал бы в несуществующий объект.
    #[test]
    fn the_dictionary_has_the_ota_object_at_its_index() {
        assert!(
            canopen::OD_TABLE
                .iter()
                .any(|entry| entry.index == super::OTA_INDEX),
            "нет объекта {:#06x} в словаре",
            super::OTA_INDEX
        );
    }
    // {%- endif %}

    fn is_heartbeat(frame: &TestFrame, node: u8) -> bool {
        frame.id == Id::Standard(StandardId::new(0x700 + u16::from(node)).unwrap())
    }

    /// По CiA 301 узел объявляет о себе кадром boot-up (`0x700 + адрес`,
    /// один байт 0) и отвечает на SDO-чтение объекта идентичности.
    #[test]
    fn boots_up_and_answers_an_sdo_upload() {
        // Чтение 0x1018:01 (vendor id): cs 0x40, индекс младшим байтом первым.
        let mut bus = Bus::with([Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0]))]);
        serve(&mut bus);
        assert_eq!(bus.sent[0], frame(0x705, &[0x00]), "первым — boot-up");
        let reply = bus
            .sent
            .iter()
            .find(|f| f.id == Id::Standard(StandardId::new(0x585).unwrap()))
            .expect("ответ SDO");
        // Ответ expedited на 4 байта: cs 0x43, тот же индекс, vendor id = 0.
        assert_eq!(reply.data, [0x43, 0x18, 0x10, 0x01, 0, 0, 0, 0]);
    }

    /// Серийный номер из входа узла — в объекте идентичности: хост читает его
    /// по SDO из `0x1018:04`.
    #[test]
    fn the_serial_reaches_the_identity_object() {
        let mut bus = Bus::with([Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x04, 0, 0, 0, 0]))]);
        serve(&mut bus);
        let reply = bus
            .sent
            .iter()
            .find(|f| f.id == Id::Standard(StandardId::new(0x585).unwrap()))
            .expect("ответ SDO");
        let mut expected = vec![0x43, 0x18, 0x10, 0x04];
        expected.extend_from_slice(&SERIAL.to_le_bytes());
        assert_eq!(reply.data, expected[..]);
    }

    /// Запись по SDO доходит до объекта словаря — того, что читает логика
    /// устройства.
    #[test]
    fn sdo_download_reaches_the_dictionary() {
        // Запись 0x2000:00 = 0x1234, expedited на 2 байта: cs 0x2B.
        let mut bus = Bus::with([Ok(frame(
            0x605,
            &[0x2B, 0x00, 0x20, 0x00, 0x34, 0x12, 0, 0],
        ))]);
        serve(&mut bus);
        let reply = bus.sent.last().expect("ответ SDO");
        assert_eq!(
            reply.data[0], 0x60,
            "подтверждение записи, а не abort: {reply:?}"
        );
        assert_eq!(canopen::OBJECT2000.get_value(), 0x1234);
    }

    /// После NMT Reset Node стек снова объявляет о себе boot-up — тоже с
    /// 0x00, а не 0x7F.
    #[test]
    fn boot_up_after_nmt_reset_is_also_zero() {
        let mut bus = Bus::with([
            // NMT Reset Node (0x81) этому узлу.
            Ok(frame(0x000, &[0x81, NODE])),
            // Любой кадр узлу — следующая обработка.
            Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0])),
        ]);
        serve(&mut bus);
        let boot_ups: Vec<_> = bus
            .sent
            .iter()
            .filter(|f| f.id == Id::Standard(StandardId::new(0x705).unwrap()))
            .collect();
        assert_eq!(
            boot_ups.len(),
            2,
            "boot-up при старте и после сброса: {:?}",
            bus.sent
        );
        assert!(boot_ups.iter().all(|f| f.data == [0x00]), "{boot_ups:?}");
    }

    /// Кадры, адресованные другим узлам, не мешают: ответ на свой запрос
    /// приходит как обычно.
    #[test]
    fn foreign_frames_are_skipped() {
        let mut bus = Bus::with([
            Ok(frame(0x706, &[0x05])),
            Ok(frame(0x186, &[1, 2, 3])),
            Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0])),
        ]);
        serve(&mut bus);
        assert_eq!(bus.sent.len(), 2, "boot-up и ответ SDO: {:?}", bus.sent);
    }

    /// Шина недоступна на отправке — узел не встаёт: запросы читаются и
    /// дальше, ответы на них пробуют уйти.
    #[test]
    fn bus_down_on_send_does_not_stop_the_node() {
        let request = || Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0]));
        let mut bus = Bus::with([request(), request()]);
        bus.send = Err(LinkError::Down("bus-off"));
        serve(&mut bus);
        assert!(bus.incoming.borrow().is_empty(), "запросы прочитаны");
        assert!(
            bus.sent
                .iter()
                .filter(|f| f.id == Id::Standard(StandardId::new(0x585).unwrap()))
                .count()
                == 2,
            "ответ на каждый: {:?}",
            bus.sent
        );
    }

    // Объекта `0x1010` нет без раздела настроек (`crates-host/canopen/build.rs`).
    // {%- if config == "true" %}
    /// `save` пишет постоянные объекты в хранилище, и новый стек над тем же
    /// хранилищем поднимается с ними, а не со значениями из `device.toml`.
    #[test]
    fn saved_objects_survive_a_new_stack() {
        let mut settings = Some(memory());
        let mut bus = Bus::with([
            // 0x2000:00 = 0x1234 (expedited, 2 байта), затем save.
            Ok(frame(0x605, &[0x2B, 0x00, 0x20, 0x00, 0x34, 0x12, 0, 0])),
            Ok(frame(0x605, &SAVE)),
        ]);
        serve_with(&mut bus, &mut settings);
        let reply = bus.sent.last().expect("ответ на save");
        assert_eq!(reply.data[0], 0x60, "save подтверждён: {reply:?}");
        let stored = stored(&mut settings, super::OBJECTS_KEY).expect("объекты сохранены");
        assert!(
            !stored.is_empty() && stored.len() <= super::STORE_CAPACITY,
            "{} байт",
            stored.len()
        );

        canopen::OBJECT2000.set_value(0);
        let mut bus = Bus::with([]);
        serve_with(&mut bus, &mut settings);
        assert_eq!(canopen::OBJECT2000.get_value(), 0x1234, "восстановлено");
        canopen::OBJECT2000.set_value(0);
    }
    // {%- endif %}

    /// Адрес, назначенный по LSS и сохранённый командой «Store
    /// configuration», новый стек берёт вместо адреса из входа узла.
    #[test]
    fn lss_address_survives_a_new_stack() {
        const ASSIGNED: u8 = 9;
        let mut settings = Some(memory());
        let mut bus = Bus::with([
            // Switch mode global → configuration, Configure node id, Store.
            Ok(frame(0x7E5, &[0x04, 0x01, 0, 0, 0, 0, 0, 0])),
            Ok(frame(0x7E5, &[0x11, ASSIGNED, 0, 0, 0, 0, 0, 0])),
            Ok(frame(0x7E5, &[0x17, 0, 0, 0, 0, 0, 0, 0])),
        ]);
        serve_with(&mut bus, &mut settings);
        assert_eq!(
            stored(&mut settings, NODE_ID_KEY),
            Some(vec![ASSIGNED]),
            "ответы LSS: {:?}",
            bus.sent
        );

        let mut bus = Bus::with([]);
        serve_with(&mut bus, &mut settings);
        assert!(
            is_heartbeat(&bus.sent[0], ASSIGNED),
            "boot-up с назначенным адресом: {:?}",
            bus.sent
        );
    }

    /// Без хранилища (`None`) колбэков нет: `save` узел принимает и работает
    /// дальше, сохранять ему некуда. Что ответит стек, зависит от прогонов до
    /// этого: флаг «сохранение поддерживается» `zencan` ставит в `static` и не
    /// снимает, а под `cargo test` тесты делят процесс, — поэтому проверяется
    /// только то, что узел жив и отвечает на следующий запрос.
    #[test]
    fn without_storage_the_node_serves_on() {
        let mut bus = Bus::with([
            Ok(frame(0x605, &SAVE)),
            Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0])),
        ]);
        serve_with(&mut bus, &mut None);
        assert!(
            bus.sent
                .iter()
                .any(|f| f.id == Id::Standard(StandardId::new(0x585).unwrap())),
            "ответ SDO: {:?}",
            bus.sent
        );
    }

    // Объекта `0x1010` нет без раздела настроек (`crates-host/canopen/build.rs`).
    // {%- if config == "true" %}
    /// `save` без размера (cs 0x22) — тоже стандартная запись: узел
    /// переставляет сигнатуру и для неё.
    #[test]
    fn save_without_size_is_accepted() {
        let mut settings = Some(memory());
        let mut save = SAVE;
        save[0] = 0x22;
        let mut bus = Bus::with([Ok(frame(0x605, &save))]);
        serve_with(&mut bus, &mut settings);
        assert!(
            stored(&mut settings, super::OBJECTS_KEY).is_some(),
            "ответы: {:?}",
            bus.sent
        );
    }
    // {%- endif %}

    /// Отказ шины на приёме узел не кончает: он снова читает шину и отвечает
    /// тем же стеком.
    #[test]
    fn a_bus_failure_does_not_stop_the_node() {
        let mut bus = Bus::with([
            Err(LinkError::Down("bus-off")),
            Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0])),
        ]);
        serve(&mut bus);
        assert!(
            bus.sent
                .iter()
                .any(|f| f.id == Id::Standard(StandardId::new(0x585).unwrap())),
            "ответ SDO после отказа: {:?}",
            bus.sent
        );
    }

    /// Серийный номер — XOR трёх слов заводского номера (little-endian).
    #[test]
    fn the_serial_folds_the_three_uid_words() {
        let uid = [1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0x80];
        assert_eq!(super::serial(uid), 0x8000_0003);
    }
}
