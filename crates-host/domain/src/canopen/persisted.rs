//! Сохранённые объекты и адрес узла: буфер, который наполняют колбэки
//! стека, и его чтение и запись в хранилище настроек.
//!
//! Мастер, настроивший узел по шине (раскладка PDO, период heartbeat,
//! уставки), пишет сигнатуру `save` в объект `0x1010` — узел сохраняет
//! постоянные объекты (`persist` в `device.toml`) в хранилище настроек под
//! ключом [`OBJECTS_KEY`], а при старте восстанавливает их. Так же адрес,
//! назначенный по LSS («Store configuration»), — под [`NODE_ID_KEY`]: при
//! следующем старте узел берёт его вместо `node_id` из графа.
//!
//! Стек зовёт колбэки синхронно изнутри `Node::process`, а хранилище
//! асинхронное: колбэк только копирует данные в буфер в RAM (он — в кадре
//! задачи, см. [`run`](crate::canopen::run)), а пишет их узел — сразу после
//! обработки, отправив ответы стека. Без хранилища (проект без раздела
//! настроек) колбэков нет вовсе, и объекта `0x1010` в словаре тоже
//! (`crates-host/canopen/build.rs`).

use core::cell::RefCell;
use core::convert::Infallible;

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::SettingsStorage;
use zencan_node::common::NodeId;
use zencan_node::embedded_io::Read;
use zencan_node::object_dict::ODEntry;
use zencan_node::{restore_stored_comm_objects, restore_stored_objects};

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
/// Живёт в кадре задачи ([`run`](crate::canopen::run)): колбэки — замыкания над ним, стек держит их
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
    pub(super) fn store(&mut self, reader: &mut dyn Read<Error = Infallible>, size: usize) {
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
    pub(super) fn store_node_id(&mut self, node_id: NodeId) {
        self.node_id = Some(node_id.raw());
    }

    /// Старт стека и NMT Reset Node: все сохранённые объекты; NMT Reset
    /// Communication (`comms`) — только объекты связи (`0x1000..=0x1FFF`).
    pub(super) fn restore(&self, od: &[ODEntry<'_>], comms: bool) {
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

/// Объекты и адрес — из хранилища в `persisted`, до создания стека; адрес —
/// если сохранён и допустим.
///
/// Отказ чтения — не отказ узла: стек стартует с адресом из графа и
/// объектами из `device.toml`, как на новой плате.
pub(super) async fn load<S>(persisted: &RefCell<Persisted>, settings: &mut S) -> Option<NodeId>
where
    S: SettingsStorage,
    S::Error: FormatOrDebug,
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
pub(super) async fn save<S>(persisted: &RefCell<Persisted>, settings: &mut S)
where
    S: SettingsStorage,
    S::Error: FormatOrDebug,
{
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
