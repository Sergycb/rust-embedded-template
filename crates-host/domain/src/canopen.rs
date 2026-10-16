//! Узел `CANOPEN` графа задач: узел CANopen (CiA 301) на шине CAN — стек
//! `zencan`.
//!
//! Стек ведёт себя так, как ждёт от него `zencan-node`: входящие кадры
//! кладутся в почтовый ящик (`NodeMbox::store_message`), периодически
//! вызывается `Node::process` — на нём heartbeat, таймауты SDO, PDO, — и всё,
//! что узел поставил в очередь передачи, уходит на шину. Узел графа делает
//! ровно это: ждёт кадр наперегонки с тактом
//! [`TICK`](crate::canopen::TICK).
//!
//! Словарь объектов — проектный: его `zencan-build` генерирует из
//! `device.toml` крейта `crates-host/canopen`, а compose-site передаёт узлу
//! ссылки на сгенерированные `static`
//! ([`Dictionary`](crate::canopen::Dictionary)). Шина —
//! [`CanBus`](ports::CanBus) из `bsp`.
//!
//! # Сохранение объектов и адреса
//!
//! Мастер, настроивший узел по шине (раскладка PDO, период heartbeat,
//! уставки), пишет сигнатуру `save` в объект `0x1010` — узел сохраняет
//! постоянные объекты (`persist` в `device.toml`) в хранилище настроек под
//! ключом [`OBJECTS_KEY`](crate::canopen::OBJECTS_KEY), а при старте восстанавливает их. Так же адрес,
//! назначенный по LSS («Store configuration»), — под
//! [`NODE_ID_KEY`](crate::canopen::NODE_ID_KEY): при
//! следующем старте узел берёт его вместо `node_id` из графа.
//!
//! Стек зовёт колбэки синхронно изнутри `Node::process`, а хранилище
//! асинхронное: колбэк только копирует данные в буфер в RAM (он — в кадре
//! задачи, см. [`run`](crate::canopen::run)), а пишет их узел — сразу после
//! обработки, отправив ответы стека. Без хранилища (проект без раздела
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

use core::cell::RefCell;
use core::convert::Infallible;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::{Duration, Instant};
use embedded_can::{ExtendedId, Frame, Id, StandardId};
use ports::{CanBus, LinkError, SettingsStorage};
use supervisor::runtime::TaskExit;
use zencan_node::common::NodeId;
use zencan_node::common::messages::{CanId, CanMessage};
use zencan_node::common::nmt::NmtState;
use zencan_node::embedded_io::Read;
use zencan_node::object_dict::ODEntry;
use zencan_node::{
    Callbacks, Node, NodeMbox, NodeState, restore_stored_comm_objects, restore_stored_objects,
};

/// Такт обработки: как часто узел зовёт `Node::process`, даже если кадров нет.
///
/// На такте держатся heartbeat (период обычно от 100 мс), таймауты SDO и
/// периодические TPDO; 10 мс — шаг, на который они могут опоздать. Чаще —
/// МК будится без дела, реже — heartbeat с периодом 100 мс уходит с
/// ощутимым дрожанием.
pub const TICK: Duration = Duration::from_millis(10);

/// Ключ хранилища настроек, под которым лежат сохранённые объекты.
///
/// Ключи узла — в своей сотне (`0xC0_0000`), чтобы не пересечься с
/// настройками проекта в том же хранилище.
pub const OBJECTS_KEY: u32 = 0x00C0_0001;

/// Ключ адреса узла, назначенного по LSS: один байт ([`NodeId::raw`]). 255 —
/// мастер сохранил «не назначен»: узел стартует ненастроенным и ждёт адреса по
/// LSS, как велит CiA 305, а не берёт адрес из графа.
pub const NODE_ID_KEY: u32 = 0x00C0_0002;

/// Сколько байт сохранённых объектов помещается.
///
/// Граница — не этот буфер, а значение хранилища настроек: у
/// `adapters::settings` рабочий буфер на 128 байт вмещает ключ (4 байта) и
/// значение. Постоянные объекты примера сериализуются в 43 байта; свой
/// `device.toml` с большим числом постоянных объектов — поднимите оба числа.
/// Не поместившееся не сохраняется (предупреждение в лог).
///
/// Подтверждение `save` по SDO стек отправляет раньше, чем объекты дойдут до
/// флеша, и сообщить мастеру об отказе записи нечем: отказ — предупреждение в
/// лог, повторит его следующий `save`.
pub const STORE_CAPACITY: usize = 96;

/// Буфер чтения хранилища: `sequential-storage` кладёт туда весь элемент —
/// ключ (4 байта) и значение, с выравниванием по слову флеша, — и отдаёт
/// значение срезом после ключа. Размер — как у рабочего буфера
/// `adapters::settings`.
const READ_SCRATCH: usize = 128;

/// Сохранённые объекты и то, что колбэки стека передали узлу на запись.
///
/// Живёт в кадре задачи ([`run`]): колбэки — замыкания над ним, стек держит их
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

    /// Старт стека и NMT Reset Node: все сохранённые объекты; NMT Reset
    /// Communication (`comms`) — только объекты связи (`0x1000..=0x1FFF`).
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
    /// Сам стек — в кадре задачи ([`run`]): состояния держат ссылку, и
    /// переход не копирует его.
    node: &'f mut Node<'f>,
    /// Момент создания стека — начало его отсчёта времени.
    start: Instant,
    persisted: &'f RefCell<Persisted>,
    /// Кадр boot-up не ушёл (шина отказала на нём): узел досылает его на
    /// следующей обработке — стек второй раз его не поставит, а NMT-мастер
    /// иначе о старте не узнал бы.
    unannounced: bool,
}

/// Сгенерированный словарь объектов: ссылки на `static` из крейта
/// `crates-host/canopen`.
#[derive(Clone, Copy)]
#[expect(
    missing_debug_implementations,
    reason = "типы zencan-node не реализуют Debug"
)]
pub struct Dictionary {
    /// `canopen::NODE_MBOX` — очереди приёма и передачи.
    pub mbox: &'static NodeMbox,
    /// `canopen::NODE_STATE` — PDO и общее состояние узла.
    pub state: &'static NodeState<'static>,
    /// `canopen::OD_TABLE` — словарь объектов.
    pub od: &'static [ODEntry<'static>],
}

// Словарь примера `crates-host/canopen` — только в проекте с CANopen: без него
// крейта словаря в проекте нет вовсе.
/// Словарь примера устройства `crates-host/canopen` — с ним узел стоит в
/// графе шаблона. Проект меняет `device.toml` того крейта, а не эту функцию.
#[must_use]
pub fn dictionary() -> Dictionary {
    Dictionary {
        mbox: &::canopen::NODE_MBOX,
        state: &::canopen::NODE_STATE,
        od: &::canopen::OD_TABLE,
    }
}

// Вход узла — `CanOpenWiring`: структуру эмитит сам фрагмент (`boot: …
// struct`). Строит её compose-site:
//
// ```ignore
// fragments: [::domain::CANOPEN_FRAG<bsp::can::Can, bsp::config::Settings> as CANOPEN
//                 = ::domain::canopen::CanOpenWiring { bus: board.can, node_id: 1,
//                       dictionary: ::domain::canopen::dictionary(),
//                       settings: board.settings, persist: true }];
// ```

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
/// Без `watchdog:` намеренно — по правилу [`ota::run`](crate::ota::run):
/// узлу нечем отметиться, кроме собственного такта, а он сторожил бы
/// исполнитель, а не работу.
///
/// # Panics
///
/// Если `node_id` во входе не 1..=127, а сохранённого адреса нет, — ошибка
/// конфигурации.
pub async fn run<C, S>(mut ctx: CanOpenCtx<'_, C, S>) -> TaskExit
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
        persisted.borrow_mut().node_id = Some(node_id.raw());
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
                Running             => frame: self.bus.recv(),
            }
            transitions {
                Booting                    => boot     -> Running,
                Running + tick(())         => tick     -> self,
                Running + frame(Ok(frame)) => received -> self,
                Running + frame(Err(err))  => failed   -> self,
            }
        }
    }

    /// Поднимает стек: адрес и сохранённые объекты — из хранилища, затем
    /// `Node::new`.
    ///
    /// Почтовый ящик — `static`: то, что в нём лежит до старта, к узлу
    /// отношения не имеет и выбрасывается.
    async fn begin(
        &mut self,
        persisted: &'f RefCell<Persisted>,
        callbacks: &'f mut Option<Callbacks<'f>>,
        node: &'f mut Option<Node<'f>>,
    ) -> Booting<'f> {
        let d = *self.dictionary;
        drop_stale(d.mbox);
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
                unannounced: false,
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

    /// `Node::process`, затем всё, что стек поставил в очередь, — на шину.
    async fn step(&mut self, s: &mut Running<'f>) {
        if s.stack.unannounced {
            self.announce(&mut s.stack).await;
        }
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
            match self.transmit(&message).await {
                Ok(sent) if announcing => s.stack.unannounced = !sent,
                Ok(_) => {}
                Err(BusDown) => {
                    // Boot-up — этот кадр или ещё в очереди за ним.
                    s.stack.unannounced |= announcing || boot_up;
                    // Остальное устарело к тому времени, как шина вернётся:
                    // мастер уйдёт по таймауту.
                    drop_stale(self.dictionary.mbox);
                }
            }
        }
        // После ответов: запись во флеш долгая, а мастер ждёт подтверждения
        // `save` по SDO. Хранилищу шина не нужна: `save` уже принят стеком.
        self.persist(s.stack.persisted).await;
    }

    /// Досылает кадр boot-up, на котором отказала шина.
    async fn announce(&mut self, stack: &mut Stack<'f>) {
        let node_id = stack.node.node_id();
        if NodeId::new(node_id).is_ok_and(|id| id.is_configured()) {
            let boot_up = CanMessage::new(CanId::std(0x700 + u16::from(node_id)), &[0x00]);
            stack.unannounced = !matches!(self.transmit(&boot_up).await, Ok(true));
        } else {
            // Адрес сняли по LSS: объявлять некого, объявит назначенный.
            stack.unannounced = false;
        }
    }

    /// Адрес узла и сохранённые объекты — из хранилища, до создания стека.
    ///
    /// Отказ чтения — не отказ узла: стек стартует с адресом из графа и
    /// объектами из `device.toml`, как на новой плате.
    ///
    /// # Panics
    ///
    /// Если `node_id` во входе не 1..=127, а сохранённого адреса нет.
    /// Без хранилища (`persist` — `false`) не читает ничего.
    async fn restore(&mut self, persisted: &RefCell<Persisted>) -> NodeId {
        if *self.persist {
            let mut scratch = [0; READ_SCRATCH];
            match self.settings.read(OBJECTS_KEY, &mut scratch).await {
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
            match self.settings.read(NODE_ID_KEY, &mut scratch).await {
                Ok(Some(&[raw])) => {
                    if let Ok(node_id) = NodeId::new(raw) {
                        return node_id;
                    }
                }
                Ok(_) => {}
                Err(err) => warn!("canopen: сохранённый адрес не прочитан: {:?}", &err),
            }
        }
        match NodeId::new(*self.node_id) {
            Ok(node_id @ NodeId::Configured(_)) => node_id,
            _ => panic!("адрес узла CANopen {} вне 1..=127", *self.node_id),
        }
    }

    /// То, что колбэки стека передали на сохранение, — в хранилище.
    #[expect(
        clippy::await_holding_refcell_ref,
        reason = "колбэки стека зовутся только из `Node::process` этой задачи, \
                  а не во время записи"
    )]
    async fn persist(&mut self, persisted: &RefCell<Persisted>) {
        let (objects, node_id) = {
            let mut persisted = persisted.borrow_mut();
            let len = persisted.len;
            let objects = core::mem::take(&mut persisted.objects_dirty).then_some(len);
            (objects, persisted.node_id.take())
        };
        if let Some(len) = objects {
            let persisted = persisted.borrow();
            match self
                .settings
                .write(OBJECTS_KEY, &persisted.objects[..len])
                .await
            {
                Ok(()) => info!("canopen: объекты сохранены, {} байт", len),
                Err(err) => warn!("canopen: объекты не сохранены: {:?}", &err),
            }
        }
        if let Some(raw) = node_id {
            match self.settings.write(NODE_ID_KEY, &[raw]).await {
                Ok(()) => info!("canopen: адрес {} сохранён", raw),
                Err(err) => warn!("canopen: адрес не сохранён: {:?}", &err),
            }
        }
    }

    /// Сообщение стека — на шину; `true` — ушло, потеря кадра — в лог.
    async fn transmit(&mut self, message: &CanMessage) -> Result<bool, BusDown> {
        let Some(frame) = to_frame::<C::Frame>(message) else {
            warn!("canopen: стек поставил в очередь кадр, который шина не примет");
            return Ok(false);
        };
        match self.bus.send(&frame).await {
            Ok(()) => Ok(true),
            Err(LinkError::Lost(err)) => {
                warn!("canopen: кадр не отправлен: {:?}", &err);
                Ok(false)
            }
            Err(LinkError::Down(err)) => {
                warn!("canopen: шина недоступна: {:?}", &err);
                Err(BusDown)
            }
        }
    }
}

/// Кадры, которые стек поставил в очередь и которые уже некому слать: до
/// старта узла (почтовый ящик — `static`) или после отказа шины.
fn drop_stale(mbox: &NodeMbox) {
    let mut stale = 0_u32;
    while mbox.next_transmit_message().is_some() {
        stale += 1;
    }
    if stale > 0 {
        warn!("canopen: выброшено {} устаревших кадров", stale);
    }
}

/// Шина отказала на отправке: остаток очереди передачи — выбросить.
struct BusDown;

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

// Узел объявлен рядом со своей задачей — как `OTA_FRAG`, `MODBUS_SLAVE_FRAG`,
// `HOST_FRAG`; в граф шаблона попадёт по выбору протокола при генерации.
//
// `<C>` и `<S>` — параметры фрагмента: шину и хранилище настроек платы
// `domain` не видит. `local` — шина и хранилище вправе быть `!Send`
// (периферия). Словарь — обычный слот: ссылки на `static` — `Send`. Стек и
// его колбэки слотов не занимают: они в кадре задачи (`run`).
supervisor::supervisor_fragment! {
    name: CANOPEN_FRAG<C, S>;
    boot: inputs: struct $crate::canopen::CanOpenWiring;

    node deps: [], restart: Never,
        resources: [
            /// Шина CAN платы.
            BUS: local C = inputs.bus,
            /// Адрес узла на шине, 1..=127.
            NODE_ID: u8 = inputs.node_id,
            /// Словарь объектов устройства.
            DICTIONARY: $crate::canopen::Dictionary = inputs.dictionary,
            /// Хранилище сохранённых объектов и адреса узла.
            SETTINGS: local S = inputs.settings,
            /// Сохранять ли объекты и адрес: `false` — хранилища у проекта нет.
            PERSIST: bool = inputs.persist
        ],
        ctx: $crate::canopen::CanOpenCtx,
        task: $crate::canopen::run;
}

// Тесты гоняют узел на словаре примера `crates-host/canopen`.
#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Waker};
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Mutex, PoisonError};

    use embassy_time::MockDriver;
    use embedded_can::{Frame, Id, StandardId};
    use ports::{CanBus, LinkError, SettingsStorage};

    use super::{CanOpenCtx, Dictionary, NODE_ID_KEY, TICK, run};

    /// Адрес узла в тестах: SDO-запросы — на `0x605`, ответы — с `0x585`.
    const NODE: u8 = 5;

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

    /// Шина: отдаёт заготовленные исходы `recv`, когда они кончаются, — ждёт
    /// вечно, как тихая шина; отправленные кадры складывает в `sent`.
    struct Bus {
        incoming: VecDeque<Result<TestFrame, LinkError<&'static str>>>,
        sent: Vec<TestFrame>,
        send: Result<(), LinkError<&'static str>>,
        /// Сколько первых отправок отвечают `Down` — шина отказала и вернулась.
        down_for: usize,
    }

    impl Bus {
        fn with(
            incoming: impl IntoIterator<Item = Result<TestFrame, LinkError<&'static str>>>,
        ) -> Self {
            Self {
                incoming: incoming.into_iter().collect(),
                sent: Vec::new(),
                send: Ok(()),
                down_for: 0,
            }
        }
    }

    impl CanBus for Bus {
        type Frame = TestFrame;
        type Error = &'static str;

        fn recv(&mut self) -> impl Future<Output = Result<TestFrame, LinkError<Self::Error>>> {
            let next = self.incoming.pop_front();
            async move {
                match next {
                    Some(outcome) => outcome,
                    None => std::future::pending().await,
                }
            }
        }

        fn send(
            &mut self,
            frame: &TestFrame,
        ) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            self.sent.push(frame.clone());
            if self.down_for > 0 {
                self.down_for -= 1;
                return std::future::ready(Err(LinkError::Down("bus-off")));
            }
            std::future::ready(self.send)
        }
    }

    /// Хранилище настроек в памяти — с поведением `sequential-storage`: в
    /// буфер чтения ложатся ключ и значение, значение отдаётся срезом после
    /// ключа, а не поместившийся элемент — ошибка.
    #[derive(Debug, Default)]
    struct Memory(HashMap<u32, Vec<u8>>);

    impl SettingsStorage for Memory {
        type Error = &'static str;

        fn read<'a>(
            &mut self,
            key: u32,
            scratch: &'a mut [u8],
        ) -> impl Future<Output = Result<Option<&'a [u8]>, Self::Error>> {
            let Some(value) = self.0.get(&key) else {
                return std::future::ready(Ok(None));
            };
            let Some(item) = scratch.get_mut(..4 + value.len()) else {
                return std::future::ready(Err("буфер меньше элемента"));
            };
            item[..4].copy_from_slice(&key.to_le_bytes());
            item[4..].copy_from_slice(value);
            std::future::ready(Ok(Some(&item[4..])))
        }

        fn write(
            &mut self,
            key: u32,
            value: &[u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            self.0.insert(key, value.to_vec());
            std::future::ready(Ok(()))
        }
    }

    fn frame(id: u16, data: &[u8]) -> TestFrame {
        TestFrame::new(StandardId::new(id).unwrap(), data).unwrap()
    }

    /// Словарь из примера — `static` на весь процесс: тесты, которые гоняют
    /// по нему узел, обязаны идти по одному, иначе один тест читает кадры
    /// другого (под `cargo test` они идут потоками одного процесса).
    static DICTIONARY: Mutex<()> = Mutex::new(());

    /// Гоняет узел на словаре из примера `crates-host/canopen`, пока кадры не
    /// кончатся: узел не выходит, а ждёт следующий.
    fn serve(bus: &mut Bus) {
        serve_with(bus, &mut Memory::default());
    }

    /// То же над заданным хранилищем: новый прогон — новый стек над ним, как
    /// после сброса МК.
    fn serve_with(bus: &mut Bus, settings: &mut Memory) {
        serve_ticks(bus, settings, true, 0);
    }

    /// То же, но ещё `ticks` раз даёт такту пройти; `persist` — есть ли у
    /// узла хранилище.
    fn serve_ticks(bus: &mut Bus, settings: &mut Memory, mut persist: bool, ticks: u32) {
        let _only_one = DICTIONARY.lock().unwrap_or_else(PoisonError::into_inner);
        let mut dictionary = Dictionary {
            mbox: &canopen::NODE_MBOX,
            state: &canopen::NODE_STATE,
            od: &canopen::OD_TABLE,
        };
        let ctx = CanOpenCtx {
            bus,
            node_id: &mut NODE.clone(),
            dictionary: &mut dictionary,
            settings,
            persist: &mut persist,
            index: 0,
        };
        let mut task = pin!(run(ctx));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт кадр");
        for _ in 0..ticks {
            MockDriver::get().advance(TICK);
            assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт кадр");
        }
    }

    /// SDO-запись `save` (`0x73 0x61 0x76 0x65`) в `0x1010:01`.
    const SAVE: [u8; 8] = [0x23, 0x10, 0x10, 0x01, 0x73, 0x61, 0x76, 0x65];

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
        assert!(bus.incoming.is_empty(), "запросы прочитаны");
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
        let mut settings = Memory::default();
        let mut bus = Bus::with([
            // 0x2000:00 = 0x1234 (expedited, 2 байта), затем save.
            Ok(frame(0x605, &[0x2B, 0x00, 0x20, 0x00, 0x34, 0x12, 0, 0])),
            Ok(frame(0x605, &SAVE)),
        ]);
        serve_with(&mut bus, &mut settings);
        let reply = bus.sent.last().expect("ответ на save");
        assert_eq!(reply.data[0], 0x60, "save подтверждён: {reply:?}");
        let stored = settings
            .0
            .get(&super::OBJECTS_KEY)
            .expect("объекты сохранены");
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
    /// configuration», новый стек берёт вместо адреса из графа.
    #[test]
    fn lss_address_survives_a_new_stack() {
        const ASSIGNED: u8 = 9;
        let mut settings = Memory::default();
        let mut bus = Bus::with([
            // Switch mode global → configuration, Configure node id, Store.
            Ok(frame(0x7E5, &[0x04, 0x01, 0, 0, 0, 0, 0, 0])),
            Ok(frame(0x7E5, &[0x11, ASSIGNED, 0, 0, 0, 0, 0, 0])),
            Ok(frame(0x7E5, &[0x17, 0, 0, 0, 0, 0, 0, 0])),
        ]);
        serve_with(&mut bus, &mut settings);
        assert_eq!(
            settings.0.get(&NODE_ID_KEY).map(Vec::as_slice),
            Some(&[ASSIGNED][..]),
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

    /// Без хранилища колбэков нет, и узел ничего не сохраняет. Что ответит
    /// стек, зависит от прогонов до этого: флаг «сохранение поддерживается»
    /// `zencan` ставит в `static` и не снимает, а под `cargo test` тесты делят
    /// процесс, — поэтому проверяется хранилище, а не ответ.
    #[test]
    fn without_storage_nothing_is_saved() {
        let mut settings = Memory::default();
        let mut bus = Bus::with([Ok(frame(0x605, &SAVE))]);
        serve_ticks(&mut bus, &mut settings, false, 0);
        assert!(settings.0.is_empty(), "{:?}", settings.0);
    }

    // Объекта `0x1010` нет без раздела настроек (`crates-host/canopen/build.rs`).
    // {%- if config == "true" %}
    /// `save` без размера (cs 0x22) — тоже стандартная запись: узел
    /// переставляет сигнатуру и для неё.
    #[test]
    fn save_without_size_is_accepted() {
        let mut settings = Memory::default();
        let mut save = SAVE;
        save[0] = 0x22;
        let mut bus = Bus::with([Ok(frame(0x605, &save))]);
        serve_with(&mut bus, &mut settings);
        assert!(
            settings.0.contains_key(&super::OBJECTS_KEY),
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

    /// Шина отказала на самом кадре boot-up: на следующем такте узел досылает
    /// его — стек второй раз его не поставит, а NMT-мастер иначе о старте не
    /// узнал бы.
    #[test]
    fn a_boot_up_lost_to_a_bus_failure_is_sent_on_the_next_tick() {
        let mut bus = Bus::with([]);
        bus.down_for = 1;
        serve_ticks(&mut bus, &mut Memory::default(), true, 1);
        let boot_ups: Vec<_> = bus.sent.iter().filter(|f| is_heartbeat(f, NODE)).collect();
        assert_eq!(boot_ups.len(), 2, "потерянный и досланный: {:?}", bus.sent);
        assert_eq!(
            boot_ups[1].data,
            [0x00],
            "досланный — boot-up, а не heartbeat"
        );
    }
}
