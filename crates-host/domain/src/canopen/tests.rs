//! Тесты узла `CANOPEN`: гоняют узел на словаре примера `crates-host/canopen`.

use core::cell::RefCell;
use core::future::poll_fn;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Mutex, PoisonError};

use embedded_can::{Frame, Id, StandardId};
use ports::{CanBus, LinkError, SettingsStorage};

use super::{NODE_ID_KEY, task};

/// Адрес узла в тестах: SDO-запросы — на `0x605`, ответы — с `0x585`.
const NODE: u8 = 5;

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

    fn write(&mut self, key: u32, value: &[u8]) -> impl Future<Output = Result<(), Self::Error>> {
        self.0.insert(key, value.to_vec());
        std::future::ready(Ok(()))
    }
}

/// Словарь из примера — `static` на весь процесс: тесты, которые гоняют
/// по нему узел, обязаны идти по одному, иначе один тест читает кадры
/// другого (под `cargo test` они идут потоками одного процесса).
static DICTIONARY: Mutex<()> = Mutex::new(());

fn frame(id: u16, data: &[u8]) -> TestFrame {
    TestFrame::new(StandardId::new(id).unwrap(), data).unwrap()
}

/// Гоняет узел на словаре из примера `crates-host/canopen`, пока кадры не
/// кончатся: узел не выходит, а ждёт следующий.
fn serve(bus: &mut Bus) {
    serve_with(bus, &mut Memory::default());
}

/// То же над заданным хранилищем: новый прогон — новый стек над ним, как
/// после сброса МК.
fn serve_with(bus: &mut Bus, settings: &mut Memory) {
    serve_persist(bus, settings, true);
}

/// То же; `persist` — есть ли у узла хранилище.
fn serve_persist(bus: &mut Bus, settings: &mut Memory, mut persist: bool) {
    let _only_one = DICTIONARY.lock().unwrap_or_else(PoisonError::into_inner);
    // Почтовый ящик — `static`: очередь передачи после прошлого прогона к
    // этому не относится.
    while canopen::NODE_MBOX.next_transmit_message().is_some() {}
    let mut dictionary = super::dictionary();
    let mut node_id = NODE;
    // {%- if ota == "true" %}
    let mut ota_client = crate::ota::ClientId(0);
    // {%- endif %}
    let stand = supervisor::stand!(CANOPEN_FRAG<Bus, Memory>);
    let ctx = stand.ctx(
        bus,
        &mut node_id,
        &mut dictionary,
        settings,
        &mut persist,
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
        let (mut node_id, mut settings, mut persist) = (NODE, Memory::default(), false);
        let mut ota_client = crate::ota::ClientId(0);
        let stand = supervisor::stand!(CANOPEN_FRAG<Bus, Memory>);
        let mut ota_node =
            crate::ota::tests::serving(stand.ota_ask.server().expect("сервер ASK свободен"));
        let ctx = stand.ctx(
            &mut bus,
            &mut node_id,
            &mut dictionary,
            &mut settings,
            &mut persist,
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
/// configuration», новый стек берёт вместо адреса из входа узла.
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
    serve_persist(&mut bus, &mut settings, false);
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
