//! Соединение и протокол на нём: порт, буферы и сроки сокета, ключ хоста,
//! вход и сессия `sunset`.

use core::pin::pin;

use defmt_or_log::{info, warn};
use embassy_futures::select::{Either, select, select3};
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use sunset_async::sunset::{ChanHandle, PubKey, ServEvent, SignKey};
use sunset_async::{ProgressHolder, SSHServer};

use crate::ssh::console::{Device, LINE, Out, parse, run, shell};
use crate::ssh::login::{Access, USER};

/// Порт SSH.
pub const PORT: u16 = 22;

/// Буфер приёма и передачи сокета, байт: полный сегмент Ethernet.
pub const TCP_BUFFER: usize = 1550;

/// Буфер пакетов SSH в каждую сторону, байт: самый длинный пакет обмена
/// ключами (`KEXINIT` клиента, открытый ключ ML-KEM) — около 1300.
pub const SSH_BUFFER: usize = 2000;

/// Простой соединения, после которого сокет закрывается. В `smoltcp` он
/// срабатывает, только пока есть неподтверждённые данные или идёт keep-alive,
/// поэтому keep-alive включён: без него молчащий клиент держал бы
/// единственное соединение узла вечно.
pub(super) const IDLE: Duration = Duration::from_secs(300);

/// Период keep-alive: клиент, пропавший без FIN (кабель, сон ноутбука),
/// отпускает соединение через `IDLE`.
pub(super) const KEEP_ALIVE: Duration = Duration::from_secs(60);

/// Сколько закрываемый сокет ждёт FIN клиента.
pub(super) const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Срок от соединения до начала сессии — обмен ключами и вход. Клиент,
/// который открыл TCP и молчит или подбирает пароль медленно, не держит
/// консоль дольше: соединение у узла одно.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Неудачных попыток входа на соединение — как `MaxAuthTries` у OpenSSH.
/// Меньше нельзя: клиент с несколькими ключами в агенте тратит попытку на
/// каждый чужой.
const MAX_AUTH_TRIES: u8 = 6;

/// Сколько протокол ведётся после конца сессии: досылка EOF и закрытия
/// канала.
const CLOSE_GRACE: Duration = Duration::from_millis(200);

/// Что клиент попросил на канале сессии.
enum Start {
    /// Интерактивная консоль.
    Shell,
    /// Одна команда.
    Exec(heapless::String<LINE>),
}

/// Протокол на принятом соединении: вход, сессия и её конец.
pub(super) async fn session(
    socket: &mut TcpSocket<'_>,
    server: SSHServer<'_>,
    key: &SignKey,
    access: Access,
    device: &Device<'_>,
) {
    let since = Instant::now();
    let (mut rx, mut tx) = socket.split();
    let mut run_protocol = pin!(server.run(&mut rx, &mut tx));
    let login = with_timeout(LOGIN_TIMEOUT, negotiate(&server, key, access, false));
    let (channel, start) = match select(&mut run_protocol, login).await {
        Either::Second(Ok(Ok(Some(begun)))) => begun,
        Either::First(_) | Either::Second(Ok(Ok(None))) => {
            info!("ssh: соединение закрыто до сессии");
            return;
        }
        Either::Second(Ok(Err(_))) => {
            warn!("ssh: ошибка протокола до сессии");
            return;
        }
        Either::Second(Err(_)) => {
            warn!("ssh: вход не уложился в срок — соединение закрыто");
            return;
        }
    };
    info!(
        "ssh: сессия через {} мс",
        Instant::now().duration_since(since).as_millis()
    );
    let Ok(io) = server.stdio(channel).await else {
        warn!("ssh: канал сессии не открылся");
        return;
    };
    // Канал закрывается, когда сессия его бросает; протокол ведётся ещё
    // `CLOSE_GRACE` — EOF и закрытие канала уходят клиенту.
    match start {
        // Команда выполняется целиком при одном `run` (сокет и вывод), а цикл
        // событий стартует после неё. EOF клиента (`ssh плата info
        // </dev/null` шлёт его сразу за `exec`) `sunset` разбирает только в
        // `progress()` и тут же отвечает своим EOF, а данные после EOF клиент
        // выбрасывает; без цикла событий EOF ждёт, и весь вывод уходит
        // раньше. Окна канала цикл событий тем временем не расширяет: вывод
        // обязан влезть в окно клиента (у OpenSSH — 2 MiB).
        Start::Exec(command) => {
            let mut out = Out { io, crlf: false };
            let done = select(&mut run_protocol, async {
                run(parse(&command), &mut out, device).await.map(|_| ())
            })
            .await;
            match done {
                // Соединение кончилось раньше команды; опрашивать
                // завершившийся `run` дальше нельзя.
                Either::First(_) => return,
                Either::Second(Err(_)) => warn!("ssh: канал сессии оборвался"),
                Either::Second(Ok(())) => {}
            }
            drop(out);
            select(
                &mut run_protocol,
                with_timeout(CLOSE_GRACE, negotiate(&server, key, access, true)),
            )
            .await;
        }
        // Консоли нужен цикл событий всё время: ввод с клавиатуры приходит
        // через `progress()`.
        Start::Shell => {
            let shell_done = async {
                if shell(io, device).await.is_err() {
                    warn!("ssh: канал сессии оборвался");
                }
                Timer::after(CLOSE_GRACE).await;
            };
            select3(
                &mut run_protocol,
                shell_done,
                negotiate(&server, key, access, true),
            )
            .await;
        }
    }
}

/// Ведёт протокол: ключ хоста, вход, канал сессии. До старта сессии
/// возвращает первый `shell`/`exec`; после (`started`) отклоняет новые
/// каналы и запросы и возвращается, только когда соединение кончилось.
async fn negotiate(
    server: &SSHServer<'_>,
    key: &SignKey,
    access: Access,
    started: bool,
) -> Result<Option<(ChanHandle, Start)>, sunset_async::sunset::Error> {
    let mut session = None;
    let mut failures = 0;
    loop {
        // `sunset` попыток не считает: перебору пароля мешает только это и
        // `LOGIN_TIMEOUT`. Отказ — конец соединения, как у OpenSSH.
        if failures >= MAX_AUTH_TRIES {
            warn!(
                "ssh: {} неудачных попыток входа — соединение закрыто",
                failures
            );
            return Ok(None);
        }
        let mut ph = ProgressHolder::new();
        // Запросы, которые не приняты явно, отклоняет их `Drop`.
        match server.progress(&mut ph).await? {
            ServEvent::Hostkeys(h) => h.hostkeys(&[key])?,
            ServEvent::FirstAuth(mut a) => a.set_auth_methods(
                matches!(access, Access::Password(_)),
                matches!(access, Access::Key(_)),
            )?,
            ServEvent::PasswordAuth(a) => {
                if let Access::Password(password) = access
                    && a.matches_username(USER)
                    && a.matches_password(password)
                {
                    a.allow()?;
                } else {
                    warn!("ssh: неверный пользователь или пароль");
                    failures += 1;
                }
            }
            ServEvent::PubkeyAuth(a) => {
                let known = match (access, a.pubkey()?) {
                    (Access::Key(key), PubKey::Ed25519(given)) => given.key.0 == key,
                    _ => false,
                };
                if known && a.username()? == USER {
                    a.allow()?;
                } else {
                    warn!("ssh: чужой ключ или пользователь");
                    failures += 1;
                }
            }
            ServEvent::Authenticated => info!("ssh: вход выполнен"),
            ServEvent::OpenSession(a) if !started && session.is_none() => {
                session = Some(a.accept()?);
            }
            ServEvent::SessionPty(a) if !started => a.succeed()?,
            ServEvent::SessionShell(a) if !started => {
                a.succeed()?;
                return Ok(session.take().map(|ch| (ch, Start::Shell)));
            }
            ServEvent::SessionExec(a) if !started => {
                let mut command = heapless::String::new();
                // Длиннее строки — такой команды всё равно нет: вместо неё
                // слово, которого нет в `parse`, и клиент получит «unknown
                // command», а не пустой ответ.
                if command.push_str(a.command()?).is_err() {
                    command.clear();
                    command.push('?').ok();
                }
                a.succeed()?;
                return Ok(session.take().map(|ch| (ch, Start::Exec(command))));
            }
            ServEvent::Defunct => return Ok(None),
            _ => {}
        }
    }
}
