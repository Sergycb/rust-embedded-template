//! Узел `CANOPEN_<транспорт>`: узел CANopen (CiA 301) на шине CAN — стек
//! `zencan`.
//!
//! Стек ведёт себя так, как ждёт от него `zencan-node`: входящие кадры
//! кладутся в почтовый ящик (`NodeMbox::store_message`), периодически
//! вызывается `Node::process` — на нём heartbeat, таймауты SDO, PDO, — и всё,
//! что узел поставил в очередь передачи, уходит на шину. Узел делает
//! ровно это: ждёт кадр наперегонки с тактом
//! [`TICK`](crate::canopen::TICK){% if ota == "true" %} и с исходом команды
//! OTA, которую отправил, не дожидаясь (`canopen::dictionary`){% endif %}.
//!
//! Словарь объектов — проектный: его `zencan-build` генерирует из
//! `device.toml` крейта `crates-host/canopen`, а compose-site передаёт узлу
//! ссылки на сгенерированные `static`
//! ([`Dictionary`](crate::canopen::Dictionary)). Шина —
//! [`CanBus`](ports::CanBus) из `bsp`. Кадр шины и сообщение стека
//! переводятся в обе стороны (`to_message`, `to_frame`); здесь же — заплатки
//! под отступления `zencan` от CiA 301 (`is_heartbeat`, `fix_save_signature`).
//!
//! # Сохранение объектов и адреса
//!
//! Мастер, настроивший узел по шине (раскладка PDO, период heartbeat,
//! уставки), пишет сигнатуру `save` в объект `0x1010` — узел сохраняет
//! постоянные объекты (`persist` в `device.toml`) в хранилище настроек под
//! ключом [`OBJECTS_KEY`](crate::canopen::OBJECTS_KEY), а при старте
//! восстанавливает их. Так же адрес, назначенный по LSS («Store
//! configuration»), — под [`NODE_ID_KEY`](crate::canopen::NODE_ID_KEY): при
//! следующем старте узел берёт его вместо `node_id` из своего входа.
//!
//! Стек зовёт колбэки синхронно изнутри `Node::process`, а хранилище
//! асинхронное: колбэк только копирует данные в буфер в RAM
//! ([`Persisted`](crate::canopen::Persisted) — в кадре задачи, см.
//! [`task`](crate::canopen::task)), а пишет их узел — сразу после обработки,
//! отправив ответы стека (`save`). Без хранилища (проект без раздела
//! настроек) колбэков нет вовсе, и объекта `0x1010` в словаре тоже
//! (`crates-host/canopen/build.rs`).
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

mod dictionary;

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
use embassy_time::{Duration, Instant};
use embedded_can::{ExtendedId, Frame, Id, StandardId};
use embedded_storage_async::nor_flash::NorFlash;
use ports::{CanBus, LinkError};
use zencan_node::common::NodeId;
use zencan_node::common::messages::{CanId, CanMessage};
use zencan_node::common::nmt::NmtState;
use zencan_node::embedded_io::Read;
use zencan_node::object_dict::ODEntry;
use zencan_node::{Callbacks, Node, restore_stored_comm_objects, restore_stored_objects};

// {%- if ota == "true" %}
use crate::ota::{ClientId, Reply, Request};
// {%- endif %}
use crate::settings::{Error as SettingsError, Settings};
use crate::tick::{deadline, next_tick};

pub use dictionary::{Dictionary, dictionary, serial};
// {%- if ota == "true" %}
pub use dictionary::{OTA, OTA_INDEX, Object};
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
        asks: [OTA.ASK: $crate::ota::Request -> $crate::ota::Reply; 1 tag ()],
        // {%- endif %}
        ctx: $crate::canopen::CanOpenCtx,
        task: $crate::canopen::task;
}

/// Такт обработки: как часто узел зовёт `Node::process`, даже если кадров нет.
///
/// На такте держатся heartbeat (период обычно от 100 мс), таймауты SDO и
/// TPDO по событию — они уходят на ближайшей обработке; 10 мс — шаг, на
/// который всё это может опоздать. Чаще —
/// МК будится без дела, реже — heartbeat с периодом 100 мс уходит с
/// ощутимым дрожанием.
pub const TICK: Duration = Duration::from_millis(10);

/// Ключ хранилища настроек, под которым лежат сохранённые объекты.
///
/// Ключи узла — в своём диапазоне `0x00C0_xxxx`, чтобы не пересечься с
/// настройками проекта в том же хранилище.
pub const OBJECTS_KEY: u32 = 0x00C0_0001;

/// Ключ адреса узла, назначенного по LSS: один байт ([`NodeId::raw`]). 255 —
/// мастер сохранил «не назначен»: узел стартует ненастроенным и ждёт адреса по
/// LSS, как велит CiA 305, а не берёт адрес из входа.
pub const NODE_ID_KEY: u32 = 0x00C0_0002;

/// Сколько байт сохранённых объектов помещается.
///
/// Граница — не этот буфер, а значение хранилища настроек: у
/// `crate::settings` рабочий буфер на 128 байт вмещает ключ (4 байта) и
/// значение. Постоянные объекты примера сериализуются в 43 байта; свой
/// `device.toml` с большим числом постоянных объектов — поднимите оба числа.
/// Не поместившееся не сохраняется (предупреждение в лог).
///
/// Подтверждение `save` по SDO стек отправляет раньше, чем объекты дойдут до
/// флеша, и сообщить мастеру об отказе записи нечем: отказ — предупреждение в
/// лог, повторит его следующий `save`.
pub const STORE_CAPACITY: usize = 96;

/// Буфер чтения хранилища: оно копирует сюда только значение. Запас сверх
/// [`STORE_CAPACITY`] — чтобы прочитать и назвать в логе длину сохранённого
/// прошивкой с большим `STORE_CAPACITY`, а не отказать на чтении.
const READ_SCRATCH: usize = 128;

// Предел самого хранилища (`Settings::MAX_VALUE`) зависит от носителя
// и проверяется в `save`.
const _: () = assert!(
    READ_SCRATCH >= STORE_CAPACITY,
    "READ_SCRATCH не вмещает STORE_CAPACITY"
);

/// Сохранённые объекты и то, что колбэки стека передали узлу на запись.
///
/// Живёт в кадре задачи ([`task`]): колбэки — замыкания над ним, стек держит их
/// всё время работы, и автомат получает ссылку аргументом стартового действия.
/// Колбэки зовутся только изнутри `Node::process` той же задачи, так что
/// `RefCell` никогда не занят дважды.
#[derive(Debug)]
pub struct Persisted {
    /// Сохранённые объекты — то, что `restore_stored_objects` читает при
    /// старте и после NMT-сброса.
    objects: [u8; STORE_CAPACITY],
    len: usize,
    /// Объекты сменились и ждут записи в хранилище.
    objects_dirty: bool,
    /// Адрес, назначенный по LSS, ждёт записи (байт [`NodeId::raw`]).
    node_id: Option<u8>,
}

impl Persisted {
    /// Пустой: ничего не сохранено и ничего не ждёт записи.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            objects: [0; STORE_CAPACITY],
            len: 0,
            objects_dirty: false,
            node_id: None,
        }
    }

    /// `0x1010 save`: постоянные объекты — в буфер; в хранилище их пишет узел.
    fn store(&mut self, reader: &mut dyn Read<Error = Infallible>, size: usize) {
        let Some(buf) = self.objects.get_mut(..size) else {
            warn!(
                "canopen: {} байт объектов не помещаются в {} — не сохранены",
                size, STORE_CAPACITY
            );
            return;
        };
        let mut filled = 0;
        while filled < size {
            match reader.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(never) => match never {},
            }
        }
        self.len = filled;
        self.objects_dirty = true;
    }

    /// LSS «Store configuration»: адрес — на запись; в хранилище его пишет узел.
    fn store_node_id(&mut self, node_id: NodeId) {
        self.node_id = Some(node_id.raw());
    }

    /// Старт стека и NMT Reset Node: все сохранённые объекты (несохранённые
    /// `zencan` 0.0.5 к умолчаниям не возвращает — `//!` модуля `canopen`);
    /// NMT Reset Communication (`comms`) — только объекты связи
    /// (`0x1000..=0x1FFF`).
    fn restore(&self, od: &[ODEntry<'_>], comms: bool) {
        if self.len == 0 {
            return;
        }
        let stored = &self.objects[..self.len];
        if comms {
            restore_stored_comm_objects(od, stored);
        } else {
            restore_stored_objects(od, stored);
        }
    }
}

impl Default for Persisted {
    fn default() -> Self {
        Self::new()
    }
}

/// Стек CANopen с тем, на чём он держится: колбэки заимствуют буфер
/// [`Persisted`] из кадра задачи на `'f`.
#[expect(
    missing_debug_implementations,
    reason = "типы zencan-node не реализуют Debug"
)]
pub struct Stack<'f> {
    /// Сам стек — в кадре задачи ([`task`]): состояния держат
    /// ссылку, и переход не копирует его.
    node: &'f mut Node<'f>,
    /// Момент создания стека — начало его отсчёта времени.
    start: Instant,
    persisted: &'f RefCell<Persisted>,
}

/// Стек в работе и ближайший такт: состояние, которое переживает круг цикла.
struct Running<'f> {
    stack: Stack<'f>,
    /// Ближайший такт.
    next: Instant,
}

/// Узел CANopen: шина, словарь, хранилище и вход `ASK` узла OTA.
struct CanOpen<'a, C, S> {
    /// Шина CAN платы.
    bus: &'a mut C,
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
    ota_ask: supervisor::Asker<'a, Request, Reply, ()>,
    // {%- endif %}
}

impl<'a, C, S> CanOpen<'a, C, S> {
    /// Собирает логику узла из полей контекста.
    #[must_use]
    fn new(
        bus: &'a mut C,
        node_id: &'a mut u8,
        dictionary: &'a mut Dictionary,
        settings: &'a mut Option<S>,
        serial: &'a mut u32,
        // {%- if ota == "true" %}
        ota_client: &'a mut ClientId,
        ota_ask: supervisor::Asker<'a, Request, Reply, ()>,
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
    async fn tick(&mut self, s: &mut Running<'_>) {
        s.next = next_tick(s.next, TICK, Instant::now());
        self.step(s).await;
    }

    /// Кадр с шины — в почтовый ящик, затем обработка.
    ///
    /// Кадр, не адресованный узлу (чужой heartbeat, PDO, SDO других узлов),
    /// почтовый ящик не принимает — он пропускается без обработки: на
    /// загруженной шине таких большинство.
    async fn received(&mut self, s: &mut Running<'_>, frame: C::Frame) {
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
    fn answered(&mut self, reply: supervisor::AskReply<(), Reply>) {
        let reply = reply.outcome.unwrap_or_else(|_dropped| {
            warn!("canopen: узел OTA не ответил");
            Err(crate::ota::commands::Rejection::Device)
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
            save(s.stack.persisted, settings).await;
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
            && let Some(node_id) = load(persisted, settings).await
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
pub async fn task<C, F>(ctx: CanOpenCtx<'_, C, Settings<F>>)
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
    let mut canopen = CanOpen::new(
        ctx.bus,
        ctx.node_id,
        ctx.dictionary,
        ctx.settings,
        ctx.serial,
        // {%- if ota == "true" %}
        ctx.ota_client,
        ctx.ota_ask,
        // {%- endif %}
    );
    let mut running = canopen.begin(&persisted, &mut callbacks, &mut node).await;
    loop {
        // Такт — первым: под потоком кадров heartbeat не должен
        // откладываться. Срок, наступивший до начала круга, обрабатывается
        // без опроса шины{% if ota == "true" %} и исхода OTA{% endif %}: как
        // в таблице `listen`, источник не опрашивается.
        if running.next <= Instant::now() {
            canopen.tick(&mut running).await;
            // Такту по наступившему сроку — круг исполнителю: иначе
            // накопившаяся очередь тактов крутила бы узел, не отдавая
            // другим задачам.
            yield_now().await;
            continue;
        }
        // Источники — в порядке приоритета: такт{% if ota == "true" %}, исход
        // OTA{% endif %}, кадр.
        // {%- if ota == "true" %}
        match select3(
            deadline(running.next),
            canopen.ota_ask.reply(),
            canopen.bus.recv(),
        )
        .await
        {
            Either3::First(()) => {
                canopen.tick(&mut running).await;
                yield_now().await;
            }
            Either3::Second(reply) => canopen.answered(reply),
            Either3::Third(result) => match result {
                Ok(frame) => canopen.received(&mut running, frame).await,
                // Кадр потерян или шина отказала — в лог, и ждём следующий
                // кадр.
                Err(LinkError::Lost(err)) => warn!("canopen: кадр потерян: {:?}", &err),
                Err(LinkError::Down(err)) => warn!("canopen: шина недоступна: {:?}", &err),
            },
        }
        // {%- else %}
        match select(deadline(running.next), canopen.bus.recv()).await {
            Either::First(()) => {
                canopen.tick(&mut running).await;
                yield_now().await;
            }
            Either::Second(result) => match result {
                Ok(frame) => canopen.received(&mut running, frame).await,
                // Кадр потерян или шина отказала — в лог, и ждём следующий
                // кадр.
                Err(LinkError::Lost(err)) => warn!("canopen: кадр потерян: {:?}", &err),
                Err(LinkError::Down(err)) => warn!("canopen: шина недоступна: {:?}", &err),
            },
        }
        // {%- endif %}
    }
}

/// Кадр heartbeat этого узла: `0x700 + адрес`, один байт состояния.
fn is_heartbeat(message: &CanMessage, node_id: u8) -> bool {
    message.id() == CanId::std(0x700 + u16::from(node_id)) && message.dlc == 1
}

/// Сигнатура `save` в SDO-записи `0x1010:01` — в том порядке байтов, которого
/// ждёт стек.
///
/// CiA 301: сигнатура — ASCII `save`, в кадре байты `73 61 76 65`, то есть
/// `u32` `0x6576_6173`. `zencan-common` 0.0.5 сравнивает с `0x7361_7665`
/// (`SAVE_CMD`) — байты наоборот, — и `save` от стандартного мастера
/// отвергает (`IncompatibleParameter`). Узел переставляет их до стека. Убрать,
/// когда исправят в zencan.
fn fix_save_signature(message: &mut CanMessage) {
    /// Индекс `0x1010` младшим байтом первым и подындекс 1.
    const STORE_OBJECT: [u8; 3] = [0x10, 0x10, 0x01];
    const SAVE: [u8; 4] = *b"save";
    let CanId::Std(id) = message.id() else {
        return;
    };
    // Expedited-запись: cs 0x23 — с размером (4 байта), 0x22 — без него.
    if (0x601..=0x67F).contains(&id)
        && message.dlc == 8
        && matches!(message.data[0], 0x22 | 0x23)
        && message.data[1..4] == STORE_OBJECT
        && message.data[4..8] == SAVE
    {
        message.data[4..8].reverse();
    }
}

/// Кадр шины → сообщение стека; `None` — кадр CAN FD длиннее 8 байт, стек
/// классический. DLC удалённого кадра стек 0.0.5 не хранит (`new_rtr`).
fn to_message(frame: &impl Frame) -> Option<CanMessage> {
    let id = match frame.id() {
        Id::Standard(id) => CanId::std(id.as_raw()),
        Id::Extended(id) => CanId::extended(id.as_raw()),
    };
    if frame.is_remote_frame() {
        Some(CanMessage::new_rtr(id))
    } else if frame.data().len() <= 8 {
        Some(CanMessage::new(id, frame.data()))
    } else {
        None
    }
}

/// Сообщение стека → кадр шины; `None` — идентификатор вне диапазона.
fn to_frame<F: Frame>(message: &CanMessage) -> Option<F> {
    let id: Id = match message.id() {
        CanId::Std(id) => StandardId::new(id)?.into(),
        CanId::Extended(id) => ExtendedId::new(id)?.into(),
    };
    if message.is_rtr() {
        F::new_remote(id, usize::from(message.dlc))
    } else {
        F::new(id, message.data())
    }
}

/// Объекты и адрес — из хранилища в `persisted`, до создания стека; адрес —
/// если сохранён и допустим.
///
/// Отказ чтения — не отказ узла: стек стартует с адресом из входа узла и
/// объектами из `device.toml`, как на новой плате.
async fn load<F>(persisted: &RefCell<Persisted>, settings: &mut Settings<F>) -> Option<NodeId>
where
    F: NorFlash,
    SettingsError<F::Error>: FormatOrDebug,
{
    let mut scratch = [0; READ_SCRATCH];
    match settings.read(OBJECTS_KEY, &mut scratch).await {
        Ok(Some(stored)) => {
            let mut persisted = persisted.borrow_mut();
            if let Some(objects) = persisted.objects.get_mut(..stored.len()) {
                objects.copy_from_slice(stored);
                persisted.len = stored.len();
            } else {
                warn!(
                    "canopen: сохранённые объекты ({} байт) больше {} — не восстановлены",
                    stored.len(),
                    STORE_CAPACITY
                );
            }
        }
        Ok(None) => {}
        Err(err) => warn!("canopen: сохранённые объекты не прочитаны: {:?}", &err),
    }
    match settings.read(NODE_ID_KEY, &mut scratch).await {
        Ok(Some(&[raw])) => NodeId::new(raw).ok(),
        Ok(_) => None,
        Err(err) => {
            warn!("canopen: сохранённый адрес не прочитан: {:?}", &err);
            None
        }
    }
}

/// То, что колбэки стека передали на сохранение, — в хранилище.
#[expect(
    clippy::await_holding_refcell_ref,
    reason = "колбэки стека зовутся только из `Node::process` этой задачи, \
              а не во время записи"
)]
async fn save<F>(persisted: &RefCell<Persisted>, settings: &mut Settings<F>)
where
    F: NorFlash,
    SettingsError<F::Error>: FormatOrDebug,
{
    // Иначе объекты записывались бы с отказом на каждом `save`.
    const {
        assert!(
            STORE_CAPACITY <= Settings::<F>::MAX_VALUE,
            "хранилище настроек не вмещает STORE_CAPACITY байт объектов CANopen"
        );
    }
    let (objects, node_id) = {
        let mut persisted = persisted.borrow_mut();
        let len = persisted.len;
        let objects = core::mem::take(&mut persisted.objects_dirty).then_some(len);
        (objects, persisted.node_id.take())
    };
    if let Some(len) = objects {
        let persisted = persisted.borrow();
        match settings.write(OBJECTS_KEY, &persisted.objects[..len]).await {
            Ok(()) => info!("canopen: объекты сохранены, {} байт", len),
            Err(err) => warn!("canopen: объекты не сохранены: {:?}", &err),
        }
    }
    if let Some(raw) = node_id {
        match settings.write(NODE_ID_KEY, &[raw]).await {
            Ok(()) => info!("canopen: адрес {} сохранён", raw),
            Err(err) => warn!("canopen: адрес не сохранён: {:?}", &err),
        }
    }
}

#[cfg(test)]
mod persisted_tests {
    use adapters_hw::flash::fake::MemFlash;
    use embedded_storage_async::nor_flash::{ErrorType, NorFlashErrorKind, ReadNorFlash};

    use super::*;

    /// Носитель настроек: две страницы по 256 байт, слово 8.
    type Flash = MemFlash<512, 8, 256>;

    /// Флеш, который отказывает на любой операции.
    struct Broken;

    impl ErrorType for Broken {
        type Error = NorFlashErrorKind;
    }

    impl ReadNorFlash for Broken {
        const READ_SIZE: usize = 1;

        fn read(
            &mut self,
            _: u32,
            _: &mut [u8],
        ) -> impl Future<Output = Result<(), NorFlashErrorKind>> {
            core::future::ready(Err(NorFlashErrorKind::Other))
        }

        fn capacity(&self) -> usize {
            512
        }
    }

    impl NorFlash for Broken {
        const WRITE_SIZE: usize = 8;
        const ERASE_SIZE: usize = 256;

        fn erase(&mut self, _: u32, _: u32) -> impl Future<Output = Result<(), NorFlashErrorKind>> {
            core::future::ready(Err(NorFlashErrorKind::Other))
        }

        fn write(
            &mut self,
            _: u32,
            _: &[u8],
        ) -> impl Future<Output = Result<(), NorFlashErrorKind>> {
            core::future::ready(Err(NorFlashErrorKind::Other))
        }
    }

    /// Пустое хранилище над стёртым флешем.
    fn memory() -> Settings<Flash> {
        Settings::new(Flash::erased(), 0..512)
    }

    /// Что записано под ключом `key`.
    async fn stored<F: NorFlash>(settings: &mut Settings<F>, key: u32) -> Option<Vec<u8>> {
        let mut scratch = [0; 128];
        settings
            .read(key, &mut scratch)
            .await
            .expect("чтение")
            .map(<[u8]>::to_vec)
    }

    #[test]
    fn store_over_capacity_keeps_nothing() {
        let data = [7; STORE_CAPACITY + 1];
        let mut persisted = Persisted::new();
        persisted.store(&mut &data[..], data.len());
        assert_eq!((persisted.len, persisted.objects_dirty), (0, false));
    }

    #[test]
    fn store_at_capacity_is_kept_and_dirty() {
        let data = [7; STORE_CAPACITY];
        let mut persisted = Persisted::new();
        persisted.store(&mut &data[..], data.len());
        assert_eq!(
            (persisted.len, persisted.objects_dirty),
            (STORE_CAPACITY, true)
        );
    }

    #[tokio::test]
    async fn load_of_a_too_large_value_restores_nothing() {
        let mut settings = memory();
        settings
            .write(OBJECTS_KEY, &[1; STORE_CAPACITY + 1])
            .await
            .expect("запись");
        let persisted = RefCell::new(Persisted::new());
        assert_eq!(load(&persisted, &mut settings).await, None);
        assert_eq!(persisted.borrow().len, 0);
    }

    #[tokio::test]
    async fn load_of_a_stored_value_and_address() {
        let mut settings = memory();
        settings
            .write(OBJECTS_KEY, &[1, 2, 3])
            .await
            .expect("запись");
        settings.write(NODE_ID_KEY, &[9]).await.expect("запись");
        let persisted = RefCell::new(Persisted::new());
        let node_id = load(&persisted, &mut settings).await;
        assert_eq!(node_id.map(|id| id.raw()), Some(9));
        assert_eq!(&persisted.borrow().objects[..3], [1, 2, 3]);
        assert_eq!(persisted.borrow().len, 3);
    }

    #[tokio::test]
    async fn load_survives_read_errors() {
        let mut settings = Settings::new(Broken, 0..512);
        let persisted = RefCell::new(Persisted::new());
        assert_eq!(load(&persisted, &mut settings).await, None);
        assert_eq!(persisted.borrow().len, 0);
    }

    #[tokio::test]
    async fn save_writes_once_and_clears_the_flags() {
        let mut settings = memory();
        let persisted = RefCell::new(Persisted::new());
        persisted.borrow_mut().store(&mut &[4, 5][..], 2);
        save(&persisted, &mut settings).await;
        assert_eq!(stored(&mut settings, OBJECTS_KEY).await, Some(vec![4, 5]));
        // Второй `save` записал бы снова; новое значение под ключом поверх
        // того же флеша показало бы, что записал.
        settings.write(OBJECTS_KEY, &[7]).await.expect("запись");
        save(&persisted, &mut settings).await;
        assert_eq!(
            stored(&mut settings, OBJECTS_KEY).await,
            Some(vec![7]),
            "второй раз писать нечего"
        );
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
    /// Объект OTA в `device.toml` стоит по индексу [`self::OTA_INDEX`]:
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
