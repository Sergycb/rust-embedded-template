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
//!   исключение в `deny.toml`. Второе отступление нашлось уже здесь: кадр
//!   boot-up уходит с байтом 0x7F вместо 0x00 — узел его исправляет (см.
//!   `boot`);
//! * `canopen-rs` (MIT/Apache) — все проверки, 0 паник, но строки и DOMAIN
//!   не длиннее 32 байт — прошивку по CAN через него не передать;
//! * `oxictl` — SDO-сервер записывает размер сегментированной передачи как
//!   значение и отвечает на abort, три параллельных словаря объектов;
//! * `funcan-rs` — заготовка без NMT/SDO/PDO; `oze-canopen`,
//!   `industrial-protocols-canopen` — требуют `std`.

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use embassy_time::{Duration, Instant};
use embedded_can::{ExtendedId, Frame, Id, StandardId};
use ports::{CanBus, LinkError};
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;
use zencan_node::common::NodeId;
use zencan_node::common::messages::{CanId, CanMessage};
use zencan_node::common::nmt::NmtState;
use zencan_node::object_dict::ODEntry;
use zencan_node::{Callbacks, Node, NodeMbox, NodeState};

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
/// [`host::MAX_LOST_IN_A_ROW`](crate::host::MAX_LOST_IN_A_ROW): шина,
/// отвечающая ошибкой на каждый приём или отправку без ожидания, иначе
/// крутила бы цикл, не отдавая исполнитель. Приём и отправка считаются
/// порознь: шина, которая принимает, но не отправляет ни одного кадра, тоже
/// отказала, и удачный приём не должен сбрасывать счёт отправки.
pub const MAX_LOST_IN_A_ROW: u8 = 32;

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

/// Что узлу нужно на входе. Строит compose-site:
///
/// ```ignore
/// // `bsp::Can` и `board.can` — реализация `CanBus` в `bsp` (этап интерфейсов).
/// fragments: [::domain::CANOPEN_FRAG<bsp::Can> as CANOPEN
///                 = ::domain::canopen::CanOpenWiring { bus: board.can, node_id: 5,
///                       dictionary: ::domain::canopen::Dictionary {
///                           mbox: &canopen::NODE_MBOX, state: &canopen::NODE_STATE, od: &canopen::OD_TABLE } }];
/// ```
#[expect(
    missing_debug_implementations,
    reason = "Dictionary держит типы zencan-node без Debug"
)]
pub struct CanOpenWiring<C> {
    /// Шина CAN платы.
    pub bus: C,
    /// Адрес узла на шине, 1..=127.
    pub node_id: u8,
    /// Словарь объектов устройства.
    pub dictionary: Dictionary,
}

/// Узел `CANOPEN`: ведёт стек CANopen, пока жива шина.
///
/// `TaskExit::Failed` — только при отказе шины (`LinkError::Down`,
/// [`MAX_LOST_IN_A_ROW`] потерь подряд); перезапуск поднимает стек заново, и
/// тот, как и положено по CiA 301, объявляет о себе кадром boot-up.
/// `Completed` не возвращается никогда.
///
/// Без `watchdog:` намеренно — по правилу [`ota::run`](crate::ota::run):
/// узлу нечем отметиться, кроме собственного такта, а он сторожил бы
/// исполнитель, а не работу.
///
/// # Panics
///
/// Если `node_id` во входе не 1..=127 — ошибка конфигурации.
pub async fn run<C>(mut ctx: CanOpenCtx<'_, C>) -> TaskExit
where
    C: CanBus,
    C::Error: Debug,
{
    ctx.run().await.result
}

#[fsm::typestate]
impl<C> CanOpenCtx<'_, C>
where
    C: CanBus,
    C::Error: Debug,
{
    fsm::machine! {
        machine CanOpen mod edges {
            states {
                /// Стек поднят, кадр boot-up ещё не отправлен; `start` —
                /// начало отсчёта времени стека.
                Booting { node: Node<'static>, start: Instant },
                /// Стек работает; `next` — ближайший такт, `lost` и `unsent` —
                /// сколько кадров подряд потеряно на приёме и на отправке.
                Running { node: Node<'static>, start: Instant, next: Instant, lost: u8, unsent: u8 },
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

    /// Поднимает стек.
    ///
    /// Почтовый ящик — `static` и переживает перезапуск узла: то, что
    /// прошлый прогон не успел отправить (ответ SDO, TPDO), к новому стеку
    /// отношения не имеет и выбрасывается.
    fn begin(&mut self) -> Booting {
        let Ok(node_id @ NodeId::Configured(_)) = NodeId::new(*self.node_id) else {
            panic!("адрес узла CANopen {} вне 1..=127", *self.node_id);
        };
        info!("domain: узел CANOPEN запущен, адрес {}", *self.node_id);
        let d = *self.dictionary;
        let mut stale = 0_u32;
        while d.mbox.next_transmit_message().is_some() {
            stale += 1;
        }
        if stale > 0 {
            warn!("canopen: выброшено {} кадров прошлого прогона", stale);
        }
        Booting {
            node: Node::new(node_id, Callbacks::default(), d.mbox, d.state, d.od),
            start: Instant::now(),
        }
    }

    /// Первая обработка: стек ставит в очередь кадр boot-up, он уходит на
    /// шину.
    async fn boot(&mut self, s: Booting) -> edges::Boot {
        let running = Running {
            node: s.node,
            start: s.start,
            next: s.start + TICK,
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
        let Some(message) = to_message(&frame) else {
            warn!(
                "canopen: кадр CAN FD на {} байт пропущен — стек классический",
                frame.data().len()
            );
            return s.into();
        };
        if self.dictionary.mbox.store_message(message).is_err() {
            return s.into();
        }
        self.step(s).await.map_or_else(Into::into, Into::into)
    }

    /// Кадр потерян на шине — ждём следующий; слишком много подряд — шина
    /// считается отказавшей.
    fn lost(&mut self, mut s: Running, err: C::Error) -> edges::Lost {
        warn!("canopen: кадр потерян: {:?}", Debug2Format(&err));
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
        // Стек объявляет о себе кадром boot-up при первом запуске, после
        // NMT Reset Node / Reset Communication и после смены адреса — в
        // обработке, которая начинается из состояния `Bootup`.
        let booting = s.node.nmt_state() == NmtState::Bootup;
        let id_before = s.node.node_id();
        // Время — от старта стека, а не от включения МК: `Node` отсчитывает
        // его с нуля, и абсолютное время дало бы на первой обработке
        // огромный интервал (лишний heartbeat, обрезка до `u32`).
        s.node.process((Instant::now() - s.start).as_micros());
        let node_id = s.node.node_id();
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
        Ok(s)
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
                warn!("canopen: кадр не отправлен: {:?}", Debug2Format(&err));
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
        warn!("canopen: шина недоступна: {:?}", Debug2Format(err));
        Down {
            result: TaskExit::Failed,
        }
    }
}

/// Кадр heartbeat этого узла: `0x700 + адрес`, один байт состояния.
fn is_heartbeat(message: &CanMessage, node_id: u8) -> bool {
    message.id() == CanId::std(0x700 + u16::from(node_id)) && message.dlc == 1
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
// `<C>` — параметр фрагмента: шину платы `domain` не видит. `local` — шина
// вправе быть `!Send` (периферия). Словарь — обычный слот: ссылки на `static`
// — `Send`.
supervisor::supervisor_fragment! {
    name: CANOPEN_FRAG<C>;
    boot: inputs: $crate::canopen::CanOpenWiring<C>;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::canopen::BACKOFF,
        resources: [
            BUS: local C = inputs.bus,
            NODE_ID: u8 = inputs.node_id,
            DICTIONARY: $crate::canopen::Dictionary = inputs.dictionary
        ],
        ctx: $crate::canopen::CanOpenCtx,
        task: $crate::canopen::run;
}

// Тесты гоняют узел на словаре примера `crates-host/canopen`.
// {%- if protocol == "canopen" %}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Mutex, PoisonError};

    use embedded_can::{Frame, Id, StandardId};
    use ports::{CanBus, LinkError};
    use supervisor::runtime::TaskExit;

    use super::{CanOpenCtx, Dictionary, MAX_LOST_IN_A_ROW, run};

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

    fn frame(id: u16, data: &[u8]) -> TestFrame {
        TestFrame::new(StandardId::new(id).unwrap(), data).unwrap()
    }

    /// Словарь из примера — `static` на весь процесс: тесты, которые гоняют
    /// по нему узел, обязаны идти по одному, иначе один тест читает кадры
    /// другого (под `cargo test` они идут потоками одного процесса).
    static DICTIONARY: Mutex<()> = Mutex::new(());

    /// Гоняет узел на словаре из примера `crates-host/canopen`, пока шина не
    /// откажет.
    fn serve(bus: &mut Bus) -> TaskExit {
        let _only_one = DICTIONARY.lock().unwrap_or_else(PoisonError::into_inner);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(serve_async(bus))
    }

    async fn serve_async(bus: &mut Bus) -> TaskExit {
        let mut dictionary = Dictionary {
            mbox: &canopen::NODE_MBOX,
            state: &canopen::NODE_STATE,
            od: &canopen::OD_TABLE,
        };
        let ctx = CanOpenCtx {
            bus,
            node_id: &mut NODE.clone(),
            dictionary: &mut dictionary,
            index: 0,
        };
        run(ctx).await
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
}
// {%- endif %}
