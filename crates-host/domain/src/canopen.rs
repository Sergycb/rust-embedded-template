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
//! асинхронное: колбэк только копирует данные в буфер в RAM, а пишет их
//! узел — сразу после обработки, отправив ответы стека. Без хранилища
//! (проект без раздела настроек) колбэков нет вовсе, и объекта `0x1010` в
//! словаре тоже (`crates-host/canopen/build.rs`).
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
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;
use zencan_node::common::NodeId;
use zencan_node::common::messages::{CanId, CanMessage};
use zencan_node::common::nmt::NmtState;
use zencan_node::critical_section::{self, Mutex};
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

/// Политика пауз между перезапусками узла — те же цифры, что у
/// [`ota::BACKOFF`](crate::ota::BACKOFF), и своя константа намеренно:
/// фрагмент самодостаточен (дать шине выйти из bus-off).
pub const BACKOFF: BackoffPolicy = BackoffPolicy {
    first: Duration::from_millis(50),
    factor: 2,
    jitter: JitterPolicy::None,
    floor: Duration::from_millis(50),
    max: Duration::from_secs(5),
};

/// Сколько потерянных кадров подряд узел терпит, прежде чем считать шину
/// отказавшей — по той же причине, что
/// `host::MAX_LOST_IN_A_ROW` у узла HOST: шина,
/// отвечающая ошибкой на каждый приём или отправку без ожидания, иначе
/// крутила бы цикл, не отдавая исполнитель. Приём и отправка считаются
/// порознь: шина, которая принимает, но не отправляет ни одного кадра, тоже
/// отказала, и удачный приём не должен сбрасывать счёт отправки.
pub const MAX_LOST_IN_A_ROW: u8 = 32;

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

/// Что колбэки стека передали узлу и что он восстанавливает: колбэки зовутся
/// изнутри `Node::process` и чужих данных не видят, поэтому — `static`, а не
/// слот узла (исключение из правила `resources:` — ради API `zencan`). Узел
/// CANopen в прошивке один. Критическая секция короткая: обход постоянных
/// объектов — десятки байт.
struct Persisted {
    /// Сохранённые объекты — то, что `restore_stored_objects` читает при
    /// старте и после NMT-сброса.
    objects: [u8; STORE_CAPACITY],
    len: usize,
    /// Объекты сменились и ждут записи в хранилище.
    objects_dirty: bool,
    /// Адрес, назначенный по LSS, ждёт записи (байт [`NodeId::raw`]).
    node_id: Option<u8>,
}

static PERSISTED: Mutex<RefCell<Persisted>> = Mutex::new(RefCell::new(Persisted {
    objects: [0; STORE_CAPACITY],
    len: 0,
    objects_dirty: false,
    node_id: None,
}));

/// Колбэки стека, которыми узел сохраняет и восстанавливает объекты.
///
/// Стек держит их ссылками `&mut` всё время жизни `Node`, поэтому объект —
/// `&'static mut` из `StaticCell` compose-site, один на прошивку, и `Node`
/// создаётся один раз и переживает перезапуски узла графа ([`Stack`]).
#[derive(Debug)]
pub struct Hooks {
    store_objects: fn(&mut dyn Read<Error = Infallible>, usize),
    store_node_config: fn(NodeId),
    reset_app: fn(&[ODEntry<'_>]),
    reset_comms: fn(&[ODEntry<'_>]),
}

impl Hooks {
    /// Колбэки узла.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            store_objects,
            store_node_config,
            reset_app: restore_all,
            reset_comms: restore_comms,
        }
    }

    fn callbacks(&'static mut self) -> Callbacks<'static> {
        Callbacks {
            store_objects: Some(&mut self.store_objects),
            store_node_config: Some(&mut self.store_node_config),
            reset_app: Some(&mut self.reset_app),
            reset_comms: Some(&mut self.reset_comms),
            ..Callbacks::default()
        }
    }
}

impl Default for Hooks {
    fn default() -> Self {
        Self::new()
    }
}

/// `0x1010 save`: постоянные объекты — в буфер; в хранилище их пишет узел.
fn store_objects(reader: &mut dyn Read<Error = Infallible>, size: usize) {
    critical_section::with(|cs| {
        let mut persisted = PERSISTED.borrow_ref_mut(cs);
        let Some(buf) = persisted.objects.get_mut(..size) else {
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
        persisted.len = filled;
        persisted.objects_dirty = true;
    });
}

/// LSS «Store configuration»: адрес — в буфер; в хранилище его пишет узел.
fn store_node_config(node_id: NodeId) {
    critical_section::with(|cs| PERSISTED.borrow_ref_mut(cs).node_id = Some(node_id.raw()));
}

/// Старт стека и NMT Reset Node: все сохранённые объекты.
fn restore_all(od: &[ODEntry<'_>]) {
    critical_section::with(|cs| {
        let persisted = PERSISTED.borrow_ref(cs);
        if persisted.len > 0 {
            restore_stored_objects(od, &persisted.objects[..persisted.len]);
        }
    });
}

/// NMT Reset Communication: только объекты связи (`0x1000..=0x1FFF`).
fn restore_comms(od: &[ODEntry<'_>]) {
    critical_section::with(|cs| {
        let persisted = PERSISTED.borrow_ref(cs);
        if persisted.len > 0 {
            restore_stored_comm_objects(od, &persisted.objects[..persisted.len]);
        }
    });
}

/// Стек CANopen — в слоте узла, а не в состоянии автомата: переживает
/// перезапуск узла графа.
///
/// Иначе нельзя: колбэки стек держит ссылками `&'static mut` ([`Hooks`]), а
/// получить такую второй раз без `unsafe` не из чего. По CiA 301 так и
/// положено: после восстановления шины узел не объявляет о себе заново, а
/// продолжает с тем же NMT-состоянием и настройками.
#[expect(
    missing_debug_implementations,
    reason = "типы zencan-node не реализуют Debug"
)]
pub struct Stack {
    /// Колбэки — до первого старта, потом они в `Node`.
    hooks: Option<&'static mut Hooks>,
    /// Стек и момент его создания — начало его отсчёта времени.
    node: Option<(Node<'static>, Instant)>,
    /// Буфер чтения и записи хранилища.
    scratch: [u8; STORE_CAPACITY],
}

impl Stack {
    /// Стек до первого старта — его собирает compose-site; `hooks` — `None`,
    /// если хранилища нет.
    #[must_use]
    pub const fn new(hooks: Option<&'static mut Hooks>) -> Self {
        Self {
            hooks,
            node: None,
            scratch: [0; STORE_CAPACITY],
        }
    }
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
// fragments: [::domain::CANOPEN_FRAG<bsp::can::Can> as CANOPEN
//                 = ::domain::canopen::CanOpenWiring { bus: board.can, node_id: 1,
//                       dictionary: ::domain::canopen::dictionary() }];
// ```

/// Узел `CANOPEN`: ведёт стек CANopen, пока жива шина.
///
/// `TaskExit::Failed` — только при отказе шины (`LinkError::Down`,
/// [`MAX_LOST_IN_A_ROW`] потерь подряд); после перезапуска стек продолжает
/// с тем же состоянием ([`Stack`]). `Completed` не возвращается никогда.
///
/// Без `watchdog:` намеренно — по правилу [`ota::run`](crate::ota::run):
/// узлу нечем отметиться, кроме собственного такта, а он сторожил бы
/// исполнитель, а не работу.
///
/// # Panics
///
/// Если `node_id` во входе не 1..=127 — ошибка конфигурации.
pub async fn run<C, S>(mut ctx: CanOpenCtx<'_, C, S>) -> TaskExit
where
    C: CanBus,
    C::Error: FormatOrDebug,
    S: SettingsStorage,
    S::Error: FormatOrDebug,
{
    ctx.run().await.result
}

#[fsm::typestate]
impl<C, S> CanOpenCtx<'_, C, S>
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
                Booting,
                /// Стек работает; `next` — ближайший такт, `lost` и `unsent` —
                /// сколько кадров подряд потеряно на приёме и на отправке.
                Running { next: Instant, lost: u8, unsent: u8 },
                /// Шина отказала: прогон окончен.
                exit Down { result: TaskExit },
            }
            start => begin -> Booting;
            listen {
                // Такт — первым: под потоком кадров heartbeat не должен
                // откладываться.
                Running { next, .. } => tick: fsm::deadline(*next),
                *                    => frame: self.bus.recv(),
            }
            transitions {
                Booting                                     => boot     -> Running | Down,
                Running + tick(())                          => tick     -> Running | Down,
                Running + frame(Ok(frame))                  => received -> Running | Down,
                Running + frame(Err(LinkError::Lost(err)))  => lost     -> Running | Down,
                Running + frame(Err(LinkError::Down(err)))  => down     -> Down,
            }
        }
    }

    /// Поднимает стек — при первом старте; после перезапуска узла стек тот
    /// же.
    ///
    /// Почтовый ящик — `static` и переживает перезапуск узла: то, что
    /// прошлый прогон не успел отправить (ответ SDO, TPDO), к новому прогону
    /// отношения не имеет и выбрасывается.
    async fn begin(&mut self) -> Booting {
        let d = *self.dictionary;
        let mut stale = 0_u32;
        while d.mbox.next_transmit_message().is_some() {
            stale += 1;
        }
        if stale > 0 {
            warn!("canopen: выброшено {} кадров прошлого прогона", stale);
        }
        if self.stack.node.is_some() {
            info!("domain: узел CANOPEN перезапущен, стек продолжает");
            return Booting;
        }
        let node_id = self.restore().await;
        info!("domain: узел CANOPEN запущен, адрес {}", node_id.raw());
        // Без колбэков — без сохранения: стек отвечает на `save` отказом.
        let callbacks = self
            .stack
            .hooks
            .take()
            .map_or_else(Callbacks::default, Hooks::callbacks);
        // `Node::new` сам зовёт `reset_app` — тот восстанавливает объекты из
        // прочитанного `restore`.
        let node = Node::new(node_id, callbacks, d.mbox, d.state, d.od);
        self.stack.node = Some((node, Instant::now()));
        Booting
    }

    /// Первая обработка: стек ставит в очередь кадр boot-up, он уходит на
    /// шину.
    async fn boot(&mut self, _: Booting) -> edges::Boot {
        let running = Running {
            next: Instant::now() + TICK,
            lost: 0,
            unsent: 0,
        };
        self.step(running).await.map_or_else(Into::into, Into::into)
    }

    /// Такт: обработка без нового кадра.
    async fn tick(&mut self, mut s: Running) -> edges::Tick {
        s.next = fsm::next_tick(s.next, TICK, Instant::now());
        self.step(s).await.map_or_else(Into::into, Into::into)
    }

    /// Кадр с шины — в почтовый ящик, затем обработка.
    ///
    /// Кадр, не адресованный узлу (чужой heartbeat, PDO, SDO других узлов),
    /// почтовый ящик не принимает — он пропускается без обработки: на
    /// загруженной шине таких большинство.
    async fn received(&mut self, mut s: Running, frame: C::Frame) -> edges::Received {
        s.lost = 0;
        let Some(mut message) = to_message(&frame) else {
            warn!(
                "canopen: кадр CAN FD на {} байт пропущен — стек классический",
                frame.data().len()
            );
            return s.into();
        };
        fix_save_signature(&mut message);
        if self.dictionary.mbox.store_message(message).is_err() {
            return s.into();
        }
        self.step(s).await.map_or_else(Into::into, Into::into)
    }

    /// Кадр потерян на шине — ждём следующий; слишком много подряд — шина
    /// считается отказавшей.
    fn lost(&mut self, mut s: Running, err: C::Error) -> edges::Lost {
        warn!("canopen: кадр потерян: {:?}", &err);
        match self.count_lost(&mut s.lost) {
            Ok(()) => s.into(),
            Err(down) => down.into(),
        }
    }

    /// Шина недоступна.
    fn down(&mut self, _: Running, err: C::Error) -> Down {
        self.bus_down(&err)
    }

    /// `Node::process`, затем всё, что стек поставил в очередь, — на шину.
    async fn step(&mut self, mut s: Running) -> Result<Running, Down> {
        let Some((node, start)) = self.stack.node.as_mut() else {
            unreachable!("стек создаёт `begin` до первой обработки");
        };
        // Стек объявляет о себе кадром boot-up при первом запуске, после
        // NMT Reset Node / Reset Communication и после смены адреса — в
        // обработке, которая начинается из состояния `Bootup`.
        let booting = node.nmt_state() == NmtState::Bootup;
        let id_before = node.node_id();
        // Время — от старта стека, а не от включения МК: `Node` отсчитывает
        // его с нуля, и абсолютное время дало бы на первой обработке
        // огромный интервал (лишний heartbeat, обрезка до `u32`).
        node.process((Instant::now() - *start).as_micros());
        let node_id = node.node_id();
        let mut boot_up = booting || node_id != id_before;
        while let Some(mut message) = self.dictionary.mbox.next_transmit_message() {
            if boot_up && is_heartbeat(&message, node_id) {
                // CiA 301: кадр boot-up — `0x700 + адрес` с байтом 0x00.
                // `zencan-node` 0.0.5 переводит узел в pre-operational до
                // отправки и шлёт 0x7F — обычный heartbeat (коммит d90a383
                // «General updates», без обоснования; до него слал 0x00), и
                // NMT-master, ждущий boot-up, не узнал бы о перезапуске узла.
                // Убрать, когда исправят в zencan.
                message.data[0] = 0x00;
                boot_up = false;
            }
            self.transmit(&message, &mut s.unsent).await?;
        }
        // После ответов: запись во флеш долгая, а мастер ждёт подтверждения
        // `save` по SDO.
        self.persist().await;
        Ok(s)
    }

    /// Адрес узла и сохранённые объекты — из хранилища, до создания стека.
    ///
    /// Отказ чтения — не отказ узла: стек стартует с адресом из графа и
    /// объектами из `device.toml`, как на новой плате.
    ///
    /// # Panics
    ///
    /// Если `node_id` во входе не 1..=127, а сохранённого адреса нет.
    async fn restore(&mut self) -> NodeId {
        let objects = match self
            .settings
            .read(OBJECTS_KEY, &mut self.stack.scratch)
            .await
        {
            Ok(Some(stored)) => stored.len(),
            Ok(None) => 0,
            Err(err) => {
                warn!("canopen: сохранённые объекты не прочитаны: {:?}", &err);
                0
            }
        };
        critical_section::with(|cs| {
            let mut persisted = PERSISTED.borrow_ref_mut(cs);
            persisted.objects[..objects].copy_from_slice(&self.stack.scratch[..objects]);
            persisted.len = objects;
            persisted.objects_dirty = false;
            persisted.node_id = None;
        });
        let stored = match self
            .settings
            .read(NODE_ID_KEY, &mut self.stack.scratch)
            .await
        {
            Ok(Some(&[raw])) => NodeId::new(raw).ok(),
            Ok(_) => None,
            Err(err) => {
                warn!("canopen: сохранённый адрес не прочитан: {:?}", &err);
                None
            }
        };
        if let Some(node_id) = stored {
            return node_id;
        }
        match NodeId::new(*self.node_id) {
            Ok(node_id @ NodeId::Configured(_)) => node_id,
            _ => panic!("адрес узла CANopen {} вне 1..=127", *self.node_id),
        }
    }

    /// То, что колбэки стека передали на сохранение, — в хранилище.
    async fn persist(&mut self) {
        let scratch = &mut self.stack.scratch;
        let (objects, node_id) = critical_section::with(|cs| {
            let mut persisted = PERSISTED.borrow_ref_mut(cs);
            let objects = core::mem::take(&mut persisted.objects_dirty).then(|| {
                let len = persisted.len;
                scratch[..len].copy_from_slice(&persisted.objects[..len]);
                len
            });
            (objects, persisted.node_id.take())
        });
        if let Some(len) = objects {
            match self
                .settings
                .write(OBJECTS_KEY, &self.stack.scratch[..len])
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

    /// Сообщение стека — на шину. Потеря кадра — в лог и в счёт потерь
    /// отправки подряд (`unsent`), отказ шины — выход.
    async fn transmit(&mut self, message: &CanMessage, unsent: &mut u8) -> Result<(), Down> {
        let Some(frame) = to_frame::<C::Frame>(message) else {
            warn!("canopen: стек поставил в очередь кадр, который шина не примет");
            return Ok(());
        };
        match self.bus.send(&frame).await {
            Ok(()) => {
                *unsent = 0;
                Ok(())
            }
            Err(LinkError::Lost(err)) => {
                warn!("canopen: кадр не отправлен: {:?}", &err);
                self.count_lost(unsent)
            }
            Err(LinkError::Down(err)) => Err(self.bus_down(&err)),
        }
    }

    /// Ещё одна потеря подряд; предел — шина считается отказавшей.
    #[expect(
        clippy::unused_self,
        reason = "общий счёт потерь для приёма и отправки"
    )]
    fn count_lost(&mut self, lost: &mut u8) -> Result<(), Down> {
        *lost = lost.saturating_add(1);
        if *lost < MAX_LOST_IN_A_ROW {
            Ok(())
        } else {
            warn!(
                "canopen: {} кадров потеряно подряд — шина не работает",
                *lost
            );
            Err(Down {
                result: TaskExit::Failed,
            })
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "общий выход для отказа на отправке и на приёме"
    )]
    fn bus_down(&mut self, err: &C::Error) -> Down {
        warn!("canopen: шина недоступна: {:?}", err);
        Down {
            result: TaskExit::Failed,
        }
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

// Узел объявлен рядом со своей задачей — как `OTA_FRAG`, `MODBUS_SLAVE_FRAG`,
// `HOST_FRAG`; в граф шаблона попадёт по выбору протокола при генерации.
//
// `<C>` и `<S>` — параметры фрагмента: шину и хранилище настроек платы
// `domain` не видит. `local` — шина и хранилище вправе быть `!Send`
// (периферия), стек — тоже (держит колбэки `&mut dyn Fn`). Словарь — обычный
// слот: ссылки на `static` — `Send`.
supervisor::supervisor_fragment! {
    name: CANOPEN_FRAG<C, S>;
    boot: inputs: struct $crate::canopen::CanOpenWiring;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::canopen::BACKOFF,
        resources: [
            /// Шина CAN платы.
            BUS: local C = inputs.bus,
            /// Адрес узла на шине, 1..=127.
            NODE_ID: u8 = inputs.node_id,
            /// Словарь объектов устройства.
            DICTIONARY: $crate::canopen::Dictionary = inputs.dictionary,
            /// Хранилище сохранённых объектов и адреса узла.
            SETTINGS: local S = inputs.settings,
            /// Стек CANopen — переживает перезапуск узла.
            STACK: local $crate::canopen::Stack = inputs.stack
        ],
        ctx: $crate::canopen::CanOpenCtx,
        task: $crate::canopen::run;
}

// Тесты гоняют узел на словаре примера `crates-host/canopen`.
#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::convert::Infallible;
    use std::sync::{Mutex, PoisonError};

    use embedded_can::{Frame, Id, StandardId};
    use ports::{CanBus, LinkError, SettingsStorage};
    use supervisor::runtime::TaskExit;

    use super::{
        CanOpenCtx, Dictionary, Hooks, MAX_LOST_IN_A_ROW, NODE_ID_KEY, OBJECTS_KEY, STORE_CAPACITY,
        Stack, run,
    };

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

    /// Шина: отдаёт заготовленные исходы `recv`, когда они кончаются, —
    /// недоступна; отправленные кадры складывает в `sent`.
    struct Bus {
        incoming: VecDeque<Result<TestFrame, LinkError<&'static str>>>,
        sent: Vec<TestFrame>,
        send: Result<(), LinkError<&'static str>>,
    }

    impl Bus {
        fn with(
            incoming: impl IntoIterator<Item = Result<TestFrame, LinkError<&'static str>>>,
        ) -> Self {
            Self {
                incoming: incoming.into_iter().collect(),
                sent: Vec::new(),
                send: Ok(()),
            }
        }
    }

    impl CanBus for Bus {
        type Frame = TestFrame;
        type Error = &'static str;

        fn recv(&mut self) -> impl Future<Output = Result<TestFrame, LinkError<Self::Error>>> {
            std::future::ready(
                self.incoming
                    .pop_front()
                    .unwrap_or(Err(LinkError::Down("шина недоступна"))),
            )
        }

        fn send(
            &mut self,
            frame: &TestFrame,
        ) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            self.sent.push(frame.clone());
            std::future::ready(self.send)
        }
    }

    /// Хранилище настроек в памяти.
    #[derive(Debug, Default)]
    struct Memory(HashMap<u32, Vec<u8>>);

    impl SettingsStorage for Memory {
        type Error = Infallible;

        fn read<'a>(
            &mut self,
            key: u32,
            scratch: &'a mut [u8],
        ) -> impl Future<Output = Result<Option<&'a [u8]>, Infallible>> {
            std::future::ready(Ok(self.0.get(&key).map(|value| {
                scratch[..value.len()].copy_from_slice(value);
                &scratch[..value.len()]
            })))
        }

        fn write(
            &mut self,
            key: u32,
            value: &[u8],
        ) -> impl Future<Output = Result<(), Infallible>> {
            self.0.insert(key, value.to_vec());
            std::future::ready(Ok(()))
        }
    }

    /// Стек с колбэками сохранения — как в графе, только `&'static mut` не
    /// из `StaticCell`, а утёкший: тесту нужен свежий на каждый прогон.
    fn stack() -> Stack {
        Stack::new(Some(Box::leak(Box::new(Hooks::new()))))
    }

    fn frame(id: u16, data: &[u8]) -> TestFrame {
        TestFrame::new(StandardId::new(id).unwrap(), data).unwrap()
    }

    /// Словарь из примера — `static` на весь процесс: тесты, которые гоняют
    /// по нему узел, обязаны идти по одному, иначе один тест читает кадры
    /// другого (под `cargo test` они идут потоками одного процесса).
    static DICTIONARY: Mutex<()> = Mutex::new(());

    /// Гоняет свежий узел на словаре из примера `crates-host/canopen`, пока
    /// шина не откажет.
    fn serve(bus: &mut Bus) -> TaskExit {
        serve_with(bus, &mut Memory::default(), &mut stack())
    }

    /// То же над заданными хранилищем и стеком: стек переживает прогон, как
    /// слот узла переживает его перезапуск.
    fn serve_with(bus: &mut Bus, settings: &mut Memory, stack: &mut Stack) -> TaskExit {
        let _only_one = DICTIONARY.lock().unwrap_or_else(PoisonError::into_inner);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
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
                stack,
                index: 0,
            };
            run(ctx).await
        })
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
        assert_eq!(serve(&mut bus), TaskExit::Failed);
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
        assert_eq!(serve(&mut bus), TaskExit::Failed);
        let reply = bus.sent.last().expect("ответ SDO");
        assert_eq!(
            reply.data[0], 0x60,
            "подтверждение записи, а не abort: {reply:?}"
        );
        assert_eq!(canopen::OBJECT2000.get_value(), 0x1234);
    }

    /// Шина, которая теряет кадр на каждом приёме, не крутит узел вечно.
    #[test]
    fn too_many_lost_frames_in_a_row_end_the_run() {
        let lost = usize::from(MAX_LOST_IN_A_ROW);
        let mut bus = Bus::with((0..lost + 5).map(|_| Err(LinkError::Lost("ошибка шины"))));
        assert_eq!(serve(&mut bus), TaskExit::Failed);
        assert_eq!(bus.incoming.len(), 5, "узел вышел после {lost} потерь");
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
        assert_eq!(serve(&mut bus), TaskExit::Failed);
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
        assert_eq!(serve(&mut bus), TaskExit::Failed);
        assert_eq!(bus.sent.len(), 2, "boot-up и ответ SDO: {:?}", bus.sent);
    }

    /// Потеря на отправке считается так же, как на приёме: шина, которая
    /// не принимает ни одного кадра, объявляется отказавшей.
    #[test]
    fn lost_sends_count_toward_the_limit() {
        let requests = usize::from(MAX_LOST_IN_A_ROW) + 5;
        let mut bus = Bus::with(
            (0..requests).map(|_| Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0]))),
        );
        bus.send = Err(LinkError::Lost("нет ACK"));
        assert_eq!(serve(&mut bus), TaskExit::Failed);
        assert!(!bus.incoming.is_empty(), "узел вышел, не дочитав запросы");
    }

    /// Шина недоступна на отправке — прогон кончается, а не теряет кадры
    /// молча.
    #[test]
    fn bus_down_on_send_ends_the_run() {
        let mut bus = Bus::with([Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0]))]);
        bus.send = Err(LinkError::Down("bus-off"));
        assert_eq!(serve(&mut bus), TaskExit::Failed);
        assert_eq!(bus.sent.len(), 1, "после отказа на boot-up — больше ничего");
        assert_eq!(bus.incoming.len(), 1, "запрос не прочитан");
    }

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
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack()),
            TaskExit::Failed
        );
        let reply = bus.sent.last().expect("ответ на save");
        assert_eq!(reply.data[0], 0x60, "save подтверждён: {reply:?}");
        let stored = settings.0.get(&OBJECTS_KEY).expect("объекты сохранены");
        assert!(
            !stored.is_empty() && stored.len() <= STORE_CAPACITY,
            "{} байт",
            stored.len()
        );

        canopen::OBJECT2000.set_value(0);
        let mut bus = Bus::with([]);
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack()),
            TaskExit::Failed
        );
        assert_eq!(canopen::OBJECT2000.get_value(), 0x1234, "восстановлено");
        canopen::OBJECT2000.set_value(0);
    }

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
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack()),
            TaskExit::Failed
        );
        assert_eq!(
            settings.0.get(&NODE_ID_KEY).map(Vec::as_slice),
            Some(&[ASSIGNED][..]),
            "ответы LSS: {:?}",
            bus.sent
        );

        let mut bus = Bus::with([]);
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack()),
            TaskExit::Failed
        );
        assert!(
            is_heartbeat(&bus.sent[0], ASSIGNED),
            "boot-up с назначенным адресом: {:?}",
            bus.sent
        );
    }

    /// Перезапуск узла графа стек не поднимает заново: boot-up — только в
    /// первом прогоне.
    #[test]
    fn a_restarted_node_keeps_its_stack() {
        let mut settings = Memory::default();
        let mut stack = stack();
        let mut bus = Bus::with([]);
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack),
            TaskExit::Failed
        );
        assert!(is_heartbeat(&bus.sent[0], NODE), "{:?}", bus.sent);

        let mut bus = Bus::with([Ok(frame(0x605, &[0x40, 0x18, 0x10, 0x01, 0, 0, 0, 0]))]);
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack),
            TaskExit::Failed
        );
        assert!(
            !bus.sent
                .iter()
                .any(|f| is_heartbeat(f, NODE) && f.data == [0x00]),
            "второго boot-up нет: {:?}",
            bus.sent
        );
        assert_eq!(bus.sent.len(), 1, "только ответ SDO: {:?}", bus.sent);
    }

    /// Без колбэков узел ничего не сохраняет. Что ответит стек, зависит от
    /// прогонов до этого: флаг «сохранение поддерживается» `zencan` ставит в
    /// `static` и не снимает (в прошивке `Node` один, и это неважно), а под
    /// `cargo test` тесты делят процесс, — поэтому проверяется хранилище, а не
    /// ответ.
    #[test]
    fn without_hooks_nothing_is_saved() {
        let mut settings = Memory::default();
        let mut bus = Bus::with([Ok(frame(0x605, &SAVE))]);
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut Stack::new(None)),
            TaskExit::Failed
        );
        assert!(settings.0.is_empty(), "{:?}", settings.0);
    }

    /// `save` без размера (cs 0x22) — тоже стандартная запись: узел
    /// переставляет сигнатуру и для неё.
    #[test]
    fn save_without_size_is_accepted() {
        let mut settings = Memory::default();
        let mut save = SAVE;
        save[0] = 0x22;
        let mut bus = Bus::with([Ok(frame(0x605, &save))]);
        assert_eq!(
            serve_with(&mut bus, &mut settings, &mut stack()),
            TaskExit::Failed
        );
        assert!(
            settings.0.contains_key(&OBJECTS_KEY),
            "ответы: {:?}",
            bus.sent
        );
    }
}
