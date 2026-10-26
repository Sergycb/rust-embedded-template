//! Клиент узла `OTA`: номер транспорта, запрос входа `CMD`/`ASK`, обёртка
//! для узла протокола и обязанность ответить тому входу, с которого пришла команда.

use ports::{OtaCommand, OtaReply};
use supervisor::{Asked, Requester, Served};

/// Ёмкость входов `CMD` и `ASK`, байт: запрос (кусок `ports::CHUNK_BYTES` со
/// смещением и длиной, тег варианта, номер клиента) и ответ. Одно число на
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
    size_of::<OtaRequest>() <= CMD_BYTES && size_of::<OtaReply>() <= CMD_BYTES,
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
pub struct OtaRequest {
    /// Кто прислал команду.
    pub client: ClientId,
    /// Сама команда.
    pub command: OtaCommand,
}

/// Клиент узла OTA для узла протокола: requester входа `CMD` и свой номер.
///
/// Узел протокола зовёт [`request`](Self::request) с голой командой, как
/// раньше `Requester::request`, а номер подставляется сам.
#[derive(Debug, Clone, Copy)]
pub struct OtaClient {
    requester: Requester<'static, OtaRequest, OtaReply>,
    id: ClientId,
}

impl OtaClient {
    /// Клиент `id` над `requester` входа `CMD`.
    #[must_use]
    pub const fn new(requester: Requester<'static, OtaRequest, OtaReply>, id: ClientId) -> Self {
        Self { requester, id }
    }

    /// Команда узлу OTA от этого клиента и её исход.
    ///
    /// # Errors
    ///
    /// [`NodeLost`] — узел взял запрос, но не ответил.
    pub async fn request(&self, command: OtaCommand) -> Result<OtaReply, NodeLost> {
        self.requester
            .request(OtaRequest {
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
    Call(Served<'a, OtaRequest, OtaReply>),
    /// Команда с входа `ASK`.
    Ask(Asked<'a, OtaRequest, OtaReply>),
}

impl Answer<'_> {
    /// Исход команды — клиенту.
    pub(super) fn respond(self, reply: OtaReply) {
        match self {
            Self::Call(served) => served.respond(reply),
            Self::Ask(asked) => asked.respond(reply),
        }
    }
}
