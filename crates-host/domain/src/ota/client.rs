//! Клиент узла `OTA`: номер транспорта, запрос входа `CMD`/`ASK`, обёртка
//! для узла протокола и обязанность ответить тому входу, с которого пришла команда.

use crate::ota::{Command, Reply};
use supervisor::{Asked, Requester, Served};

/// Ёмкость входов `CMD` и `ASK`, байт: запрос (кусок
/// [`CHUNK_BYTES`](crate::ota::commands::CHUNK_BYTES) со смещением и длиной,
/// тег варианта, номер клиента) и ответ. Одно число на
// {%- if graph == "true" %}
/// граф (`request:` и `serves:` в `OTA_FRAG`). Во фрагменте оно записано
/// литералом — макрос не принимает путь, — так что меняется в трёх местах.
// {%- else %}
/// фрагмент (`request:` и `serves:` в `OTA_FRAG`) и на `static` входов
/// (`crates-cross/app/src/tasks/ota.rs`). Во фрагменте оно записано
/// литералом — макрос не принимает путь, — так что меняется во всех этих
/// местах вместе.
// {%- endif %}
pub const CMD_BYTES: usize = 144;

const _: () = assert!(
    size_of::<Request>() <= CMD_BYTES && size_of::<Reply>() <= CMD_BYTES,
    "команда OTA не помещается во вход CMD: поднимите CMD_BYTES"
);

/// Номер клиента входа `CMD` — транспорта, с которого пришла команда.
///
/// Транспорт, а не удалённый хост: два хоста на одной шине для узла — один
/// клиент. Номера раздаёт compose-site (`app`), по одному на узел протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ClientId(pub u8);

/// Запрос на вход `CMD`: команда и клиент, от которого она пришла.
///
/// Клиент нужен автомату, чтобы приём образа принадлежал одному транспорту:
/// пока идёт приём, `Start`/`Chunk`/`Finish` другого клиента получают
/// `Busy`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Request {
    /// Кто прислал команду.
    pub client: ClientId,
    /// Сама команда.
    pub command: Command,
}

/// Клиент узла OTA для узла протокола: requester входа `CMD` и свой номер.
///
/// Узел протокола зовёт [`request`](Self::request) с голой командой; номер
/// клиента подставляется сам.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    requester: Requester<'static, Request, Reply>,
    id: ClientId,
}

impl Client {
    /// Клиент `id` над `requester` входа `CMD`.
    #[must_use]
    pub const fn new(requester: Requester<'static, Request, Reply>, id: ClientId) -> Self {
        Self { requester, id }
    }

    /// Команда узлу OTA от этого клиента и её исход.
    ///
    /// # Errors
    ///
    /// [`NodeLost`] — узел взял запрос, но не ответил.
    pub async fn request(&self, command: Command) -> Result<Reply, NodeLost> {
        self.requester
            .request(Request {
                client: self.id,
                command,
            })
            .await
            // `RequestDroppedError` — единичный тип: причины, которую стоило бы
            // хранить, в нём нет.
            .map_err(|_dropped| NodeLost)
    }
}

/// Узел OTA взял команду, но ответа на неё не будет: задача узла
/// прервалась посреди обработки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NodeLost;

impl core::fmt::Display for NodeLost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("узел OTA не ответил на команду")
    }
}

impl core::error::Error for NodeLost {}

/// Обязанность ответить на команду — тому входу, с которого она пришла.
///
/// Действиям автомата всё равно, ждёт ли клиент в вызове (`CMD`) или забирает
/// исход позже (`ASK`): ответ один и тот же, дроп без ответа у обоих —
/// «узел не ответил».
#[derive(Debug)]
pub(super) enum Answer<'a> {
    /// Команда с входа `CMD`.
    Call(Served<'a, Request, Reply>),
    /// Команда с входа `ASK`.
    Ask(Asked<'a, Request, Reply>),
}

impl Answer<'_> {
    /// Исход команды — клиенту.
    pub(super) fn respond(self, reply: Reply) {
        match self {
            Self::Call(served) => served.respond(reply),
            Self::Ask(asked) => asked.respond(reply),
        }
    }
}

/// Мест ожидания входа `CMD` на `clients` его клиентов — параметр `PARKED`
/// фрагмента `OTA_FRAG`; число клиентов знает только compose-site.
///
/// Клиенты — заранее известный набор, по одному на транспорт: один держит
/// вызов, ждут остальные, так что мест на одно меньше клиентов — тогда
/// ждущие не будят друг друга по кругу (`sync_request::PARKED_CALLERS`,
/// документация). Не меньше одного: ноль мест не компилируется, а одного
/// хватает и на двух клиентов — в том числе тех, что проект без протокола
/// подключит к входу сам.
///
/// ```
/// use domain::ota::cmd_parked;
///
/// assert_eq!(cmd_parked(0), 1);
/// assert_eq!(cmd_parked(2), 1);
/// assert_eq!(cmd_parked(6), 5);
/// ```
#[must_use]
pub const fn cmd_parked(clients: usize) -> usize {
    if clients > 2 { clients - 1 } else { 1 }
}
