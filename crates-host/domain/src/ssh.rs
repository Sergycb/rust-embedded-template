//! Узел `SSH`: консоль платы по SSH.
//!
//! Одно соединение на порту [`PORT`](crate::ssh::PORT), пользователь
//! [`USER`](crate::ssh::USER), одна сессия: `shell` или `exec`. Вход —
//! [`Login`](crate::ssh::Login): по паролю или по открытому ключу ed25519, что выбрано при
//! генерации. Команды:
//!
//! | Команда | Ответ |
//! |---|---|
//! | `help` | список команд |
//! | `info` | прошивка, чип, заводской номер, MAC, адрес, время работы |
// {%- if ota == "true" %}
//! | `reboot yes` | перезагрузка — командой узлу OTA; без `yes` — подсказка |
// {%- endif %}
//! | `exit` | закрывает сессию (как и EOF, Ctrl-D) |
//!
//! Это отправная точка: свои команды проект добавляет в [`Command`](crate::ssh::Command),
//! `parse` и `run`, данные берёт у своих узлов клиентами их входов, как
//! перезагрузку — у OTA.
//!
//! Протокол — `sunset`, сокет — `sunset-async` над `embassy-net`. Ключ хоста
//! ed25519 даёт [`HostKey`](crate::ssh::HostKey): вшитый в прошивку ([`Fixed`](crate::ssh::Fixed)) или созданный
//! платой при первом старте и хранимый в разделе настроек (`Stored`). Он не
//! меняется от сброса к сбросу, и клиент один раз запоминает его в
//! `known_hosts`. Попыток входа `sunset` не считает — их ограничивает узел: шесть
//! неудач и 30 секунд на вход на соединение.
//!
//! Чего в `sunset` 0.6 нет: `exit-status` (OpenSSH выходит с кодом 255) и
//! EOF/CLOSE канала по инициативе сервера — оба `sunset` шлёт только эхом на
//! пакеты клиента. Поэтому соединение после сессии закрывает узел, а вывод
//! `exec` встаёт в очередь раньше, чем разбирается EOF клиента (см.
//! `session`).

use core::pin::pin;

use defmt_or_log::{info, warn};
use embassy_futures::select::{Either, select, select3};
use embassy_net::Stack;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::{Read, Write};
use sunset_async::sunset::ed25519_dalek::SigningKey;
use sunset_async::sunset::{ChanHandle, PubKey, ServEvent, SignKey};
use sunset_async::{ChanInOut, ProgressHolder, SSHServer};
use supervisor::runtime::TaskExit;
// {%- if ota == "true" %}
use ports::{OtaCommand, Progress};

use crate::ota::OtaClient;
// {%- endif %}
use crate::device::{DeviceInfo, PANIC_BRIEF};

/// Порт SSH.
pub const PORT: u16 = 22;

/// Пользователь консоли.
pub const USER: &str = "admin";

/// Буфер приёма и передачи сокета, байт: полный сегмент Ethernet.
pub const TCP_BUFFER: usize = 1550;

/// Буфер пакетов SSH в каждую сторону, байт: самый длинный пакет обмена
/// ключами (`KEXINIT` клиента, открытый ключ ML-KEM) — около 1300.
pub const SSH_BUFFER: usize = 2000;

/// Простой соединения, после которого сокет закрывается. В `smoltcp` он
/// срабатывает, только пока есть неподтверждённые данные или идёт keep-alive,
/// поэтому keep-alive включён: без него молчащий клиент держал бы
/// единственное соединение узла вечно.
const IDLE: Duration = Duration::from_secs(300);

/// Период keep-alive: клиент, пропавший без FIN (кабель, сон ноутбука),
/// отпускает соединение через `IDLE`.
const KEEP_ALIVE: Duration = Duration::from_secs(60);

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

/// Сколько закрываемый сокет ждёт FIN клиента.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Длина строки команды.
const LINE: usize = 64;

/// Самый длинный ответ на `info`, байт: 256 — прошлые строки с запасом под
/// длинную версию, 27 — `reset independent_watchdog`, остальное — строка
/// `last panic` с [`PANIC_BRIEF`] байт.
const INFO_TEXT: usize = 256 + 27 + "last panic \n".len() + PANIC_BRIEF;
// {%- if ssh_host_key == "config" %}

/// Ключ хоста в разделе настроек.
pub const HOST_KEY_ID: u32 = 0x0055_0001;
// {%- endif %}

/// Как плата пускает пользователя [`USER`].
#[derive(Debug, Clone, Copy)]
pub enum Login {
    /// По паролю.
    Password(&'static str),
    /// По открытому ключу: строка `ssh-ed25519 AAAA… [комментарий]` из
    /// `~/.ssh/id_ed25519.pub`.
    Key(&'static str),
}

/// Ключ хоста — зерно ed25519, 32 байта.
// Future без границы `Send` — то, что нужно однопоточному исполнителю
// embassy; так же, как трейты `ports` (`ports/src/lib.rs`).
#[expect(
    async_fn_in_trait,
    reason = "узел SSH зовёт ключ на одном исполнителе: `Send` у future не нужен"
)]
pub trait HostKey {
    /// Зерно ключа хоста. Не отказывает: ключ нужен, чтобы вообще пустить
    /// клиента, и реализация, которой не удалось сохранить ключ, отдаёт его
    /// хотя бы на этот запуск.
    async fn seed(&mut self) -> [u8; 32];
}

/// Ключ хоста, вшитый в прошивку.
#[derive(Debug, Clone, Copy)]
pub struct Fixed(pub &'static [u8; 32]);

/// Собирать нечего: зерно вшито в образ, compose-site отдаёт ссылку на него.
impl ports::Assemble for Fixed {
    type Source = &'static [u8; 32];
    type Buffers = ();

    fn assemble(seed: &'static [u8; 32], (): ()) -> Self {
        Self(seed)
    }
}

impl HostKey for Fixed {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт трейт `HostKey`; вшитому ключу ждать нечего"
    )]
    async fn seed(&mut self) -> [u8; 32] {
        *self.0
    }
}
// {%- if ssh_host_key == "config" %}

/// Ключ хоста в хранилище настроек: первый старт создаёт его из `getrandom`
/// и записывает под [`HOST_KEY_ID`], следующие — читают.
#[derive(Debug)]
pub struct Stored<S>(pub S);

/// Собирается вместе с хранилищем — над тем же рычагом платы.
impl<S: ports::Assemble> ports::Assemble for Stored<S> {
    type Source = S::Source;
    type Buffers = S::Buffers;

    fn assemble(source: S::Source, buffers: S::Buffers) -> Self {
        Self(S::assemble(source, buffers))
    }
}

impl<S: ports::SettingsStorage> HostKey for Stored<S> {
    async fn seed(&mut self) -> [u8; 32] {
        let mut scratch = [0; 32];
        // Записывается новый ключ, только когда прежнего точно нет. Раздел не
        // прочитался — ключ на этот запуск, без записи: сбой мог быть
        // временным, а запись поверх сменила бы ключ навсегда, и клиенты
        // увидели бы «REMOTE PROTOBUF IDENTIFICATION HAS CHANGED».
        let keep = match self.0.read(HOST_KEY_ID, &mut scratch).await {
            Ok(Some(stored)) => {
                if let Ok(seed) = <[u8; 32]>::try_from(stored) {
                    return seed;
                }
                warn!("ssh: ключ хоста в настройках не 32 байта — создаётся новый");
                true
            }
            Ok(None) => {
                info!("ssh: ключа хоста нет — создаётся");
                true
            }
            Err(_) => {
                warn!("ssh: ключ хоста не прочитался — временный, до сброса");
                false
            }
        };
        let mut seed = [0; 32];
        if getrandom::fill(&mut seed).is_err() {
            // Ключа нет — нет и консоли; нулевое зерно отдать нельзя: его
            // знает каждый.
            warn!("ssh: нет случайных байтов для ключа хоста");
            core::future::pending::<()>().await;
        }
        if keep && self.0.write(HOST_KEY_ID, &seed).await.is_err() {
            warn!("ssh: ключ хоста не сохранился — после сброса он сменится");
        }
        seed
    }
}
// {%- endif %}

/// Кого пускать: проверенная форма [`Login`].
#[derive(Clone, Copy)]
enum Access {
    Password(&'static str),
    Key([u8; 32]),
    /// Ключ из прошивки не разобрался — никого: пускать без проверки нельзя.
    Nobody,
}

impl Access {
    fn new(login: Login) -> Self {
        match login {
            Login::Password(password) => Self::Password(password),
            Login::Key(line) => {
                if let Some(key) = ed25519_public_key(line) {
                    Self::Key(key)
                } else {
                    warn!("ssh: ключ пользователя в прошивке не ed25519 — вход закрыт");
                    Self::Nobody
                }
            }
        }
    }
}

/// Команда консоли.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Пустая строка.
    Empty,
    /// Список команд.
    Help,
    /// Сведения об устройстве.
    Info,
    // {%- if ota == "true" %}
    /// Перезагрузка; без подтверждения — только подсказка.
    Reboot {
        /// `reboot yes`.
        confirmed: bool,
    },
    // {%- endif %}
    /// Конец сессии.
    Exit,
    /// Такой команды нет.
    Unknown,
}

/// Что команды знают об устройстве.
struct Device<'a> {
    info: &'a DeviceInfo,
    stack: Stack<'static>,
    // {%- if ota == "true" %}
    ota: OtaClient,
    // {%- endif %}
}

/// Вывод в канал: терминалу (`shell`) строки уходят с `\r\n`, `exec` — как
/// есть.
struct Out<W> {
    io: W,
    crlf: bool,
}

impl<W: Write> Out<W> {
    async fn text(&mut self, text: &str) -> Result<(), W::Error> {
        if !self.crlf {
            return self.io.write_all(text.as_bytes()).await;
        }
        let mut lines = text.split('\n');
        if let Some(first) = lines.next() {
            self.io.write_all(first.as_bytes()).await?;
        }
        for line in lines {
            self.io.write_all(b"\r\n").await?;
            self.io.write_all(line.as_bytes()).await?;
        }
        Ok(())
    }
}

/// Что клиент попросил на канале сессии.
enum Start {
    /// Интерактивная консоль.
    Shell,
    /// Одна команда.
    Exec(heapless::String<LINE>),
}

#[fsm::typestate]
impl<K: HostKey> SshCtx<'_, K> {
    fsm::machine! {
        machine Ssh mod edges {
            states {
                /// Ждёт соединения: ключ хоста и кого пускать.
                Listening { key: SignKey, access: Access },
                /// Недостижимо: сервер не возвращается. Состояние нужно
                /// автомату — без выхода задача не вернула бы `TaskExit`.
                exit Down { result: TaskExit },
            }
            start => begin -> Listening;
            transitions {
                Listening => serve -> Down,
            }
        }
    }

    async fn begin(&mut self) -> Listening {
        info!("domain: узел SSH запущен");
        let seed = self.host_key.seed().await;
        Listening {
            key: SignKey::Ed25519(SigningKey::from_bytes(&seed)),
            access: Access::new(*self.login),
        }
    }

    /// Принимает соединение, ведёт его до конца, ждёт следующее —
    /// бесконечно.
    async fn serve(&mut self, s: &Listening) -> Down {
        loop {
            self.connection(&s.key, s.access).await;
        }
    }

    /// Одно соединение. Отказ соединения или протокола узел пишет в лог и
    /// слушает дальше.
    async fn connection(&mut self, key: &SignKey, access: Access) {
        let stack = *self.stack;
        let mut socket = TcpSocket::new(stack, &mut self.tcp_rx[..], &mut self.tcp_tx[..]);
        socket.set_timeout(Some(IDLE));
        socket.set_keep_alive(Some(KEEP_ALIVE));
        if socket.accept(PORT).await.is_err() {
            warn!("ssh: accept не удался");
            return;
        }
        if let Some(peer) = socket.remote_endpoint() {
            info!("ssh: соединение от {}", peer);
        }
        let device = Device {
            info: self.info,
            stack,
            // {%- if ota == "true" %}
            ota: *self.ota,
            // {%- endif %}
        };
        session(
            &mut socket,
            SSHServer::new(&mut self.ssh_rx[..], &mut self.ssh_tx[..]),
            key,
            access,
            &device,
        )
        .await;
        // Соединение закрывает сервер: `sunset` закрывает канал, но не
        // соединение, а клиент ждёт именно его. Сокет бросается только после
        // FIN клиента: брошенный раньше ответит на него RST, и клиент под
        // Windows теряет ещё не прочитанный вывод.
        socket.close();
        // Хвост вывода и наш FIN уходят раньше, чем ждётся FIN клиента:
        // сокет, брошенный с неподтверждёнными данными, их не досылает.
        let drained = with_timeout(CLOSE_WAIT, async {
            socket.flush().await.ok();
            let mut sink = [0; 64];
            while let Ok(1..) = socket.read(&mut sink).await {}
        })
        .await;
        if drained.is_err() {
            socket.abort();
        }
        info!("ssh: соединение закрыто");
    }
}

/// Вход узла `SSH`: стек, сведения, вход пользователя и из чего собрать ключ
/// хоста `K` (`Fixed` — вшитый ключ, `Stored` — над хранилищем настроек).
///
/// Написан руками, а не выведен фрагментом: слот ключа инициализируется
/// сборкой (порт [`Assemble`](ports::Assemble)), а не чтением поля.
pub struct SshWiring<K: ports::Assemble> {
    /// Сетевой стек, на котором узел открывает свой сокет.
    pub stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    pub info: crate::device::DeviceInfo,
    /// Как пускать пользователя.
    pub login: Login,
    /// Из чего собрать ключ хоста.
    pub host_key: K::Source,
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `reboot`.
    pub ota: crate::ota::OtaClient,
    // {%- endif %}
}

// Вручную: у стека `embassy-net` нет `Debug`.
impl<K: ports::Assemble> core::fmt::Debug for SshWiring<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SshWiring").finish_non_exhaustive()
    }
}

/// Открытый ключ ed25519 из строки `ssh-ed25519 <base64> [комментарий]`: блоб
/// — имя типа и 32 байта ключа, каждое с длиной впереди. `None` — строка не
/// такая.
#[must_use]
pub fn ed25519_public_key(line: &str) -> Option<[u8; 32]> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "ssh-ed25519" {
        return None;
    }
    let mut blob = [0; 51];
    if base64(words.next()?.as_bytes(), &mut blob)? != blob.len() {
        return None;
    }
    let (name, key) = blob.split_at(4 + 11);
    if name != b"\0\0\0\x0bssh-ed25519" || key[..4] != [0, 0, 0, 32] {
        return None;
    }
    key[4..].try_into().ok()
}

/// Декодирует base64 (стандартный алфавит, `=` в конце) в начало `out`;
/// длина результата или `None` — не base64 либо не помещается.
fn base64(text: &[u8], out: &mut [u8]) -> Option<usize> {
    let text = text
        .strip_suffix(b"==")
        .or(text.strip_suffix(b"="))
        .unwrap_or(text);
    let (mut acc, mut bits, mut len) = (0u32, 0, 0);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(len)? = (acc >> bits).to_le_bytes()[0];
            len += 1;
            // Отданные биты — прочь: иначе сдвиг переполнил бы `acc` на
            // шестом символе.
            acc &= (1 << bits) - 1;
        }
    }
    Some(len)
}

/// Команда из строки.
fn parse(line: &str) -> Command {
    let mut words = line.split_ascii_whitespace();
    let command = match (words.next(), words.next()) {
        (None, _) => Command::Empty,
        (Some("help"), None) => Command::Help,
        (Some("info"), None) => Command::Info,
        // {%- if ota == "true" %}
        (Some("reboot"), None) => Command::Reboot { confirmed: false },
        (Some("reboot"), Some("yes")) => Command::Reboot { confirmed: true },
        // {%- endif %}
        (Some("exit"), None) => Command::Exit,
        _ => Command::Unknown,
    };
    if words.next().is_some() {
        Command::Unknown
    } else {
        command
    }
}

/// Сведения об устройстве текстом.
fn write_info(
    out: &mut impl core::fmt::Write,
    info: &DeviceInfo,
    mac: [u8; 6],
    ip: Option<embassy_net::Ipv4Address>,
    uptime_s: u64,
) -> core::fmt::Result {
    writeln!(out, "version {} ({})", info.version, info.commit)?;
    write!(
        out,
        "chip {}, flash {} KiB",
        info.chip,
        info.flash_size / 1024
    )?;
    match info.sys_hz {
        Some(hz) => writeln!(out, ", core {} MHz", hz / 1_000_000)?,
        None => out.write_char('\n')?,
    }
    out.write_str("uid ")?;
    for b in info.uid {
        write!(out, "{b:02x}")?;
    }
    out.write_str("\nmac ")?;
    for (i, b) in mac.iter().enumerate() {
        write!(out, "{}{b:02x}", if i == 0 { "" } else { ":" })?;
    }
    match ip {
        Some(ip) => writeln!(out, "\nip {ip}")?,
        None => out.write_str("\nip none\n")?,
    }
    writeln!(out, "uptime {uptime_s} s")?;
    writeln!(out, "reset {}", info.reset.as_str())?;
    // Место и текст паники разделяет её же перевод строки: текст — следующей
    // строкой.
    writeln!(out, "last panic {}", info.last_panic.unwrap_or("none"))
}

/// Выполняет команду; `false` — сессия кончилась (`exit`).
async fn run<W: Write>(
    command: Command,
    out: &mut Out<W>,
    device: &Device<'_>,
) -> Result<bool, W::Error> {
    match command {
        Command::Empty => {}
        Command::Help => {
            out.text("help            this list\n").await?;
            out.text("info            firmware, chip, unique id, MAC, address, uptime,\n")
                .await?;
            out.text("                reset cause, last panic\n")
                .await?;
            // {%- if ota == "true" %}
            out.text("reboot yes      reboot the board\n").await?;
            // {%- endif %}
            out.text("exit            close the session (also Ctrl-D)\n")
                .await?;
        }
        Command::Info => {
            let mut text = heapless::String::<INFO_TEXT>::new();
            // Среда в стеке одна — Ethernet (`medium-ethernet`).
            let embassy_net::HardwareAddress::Ethernet(mac) = device.stack.hardware_address();
            let ip = device
                .stack
                .config_v4()
                .map(|config| config.address.address());
            let uptime_s = Instant::now().as_secs();
            match write_info(&mut text, device.info, mac.0, ip, uptime_s) {
                Ok(()) => out.text(&text).await?,
                Err(core::fmt::Error) => out.text("info too long\n").await?,
            }
        }
        // {%- if ota == "true" %}
        Command::Reboot { confirmed: false } => out.text("to reboot, type: reboot yes\n").await?,
        Command::Reboot { confirmed: true } => {
            // Узел OTA отвечает раньше сброса: ответ успевает уйти.
            let reply = match device.ota.request(OtaCommand::Reboot).await {
                Ok(Ok(Progress::Rebooting)) => "rebooting\n",
                Ok(_) => "reboot refused\n",
                Err(_) => "ota node lost\n",
            };
            out.text(reply).await?;
        }
        // {%- endif %}
        Command::Exit => return Ok(false),
        Command::Unknown => out.text("unknown command, 'help' for the list\n").await?,
    }
    Ok(true)
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

/// Консоль `shell`: строка за строкой до `exit`, Ctrl-D или EOF.
async fn shell(io: ChanInOut<'_>, device: &Device<'_>) -> Result<(), sunset_async::sunset::Error> {
    let mut out = Out { io, crlf: true };
    out.text("console, 'help' for commands\n> ").await?;
    let mut line = heapless::String::<LINE>::new();
    let mut byte = [0];
    let mut after_cr = false;
    loop {
        if out.io.read(&mut byte).await? == 0 {
            return Ok(());
        }
        // `\r\n` — один конец строки, а не строка и пустая за ней.
        let skip = after_cr && byte[0] == b'\n';
        after_cr = byte[0] == b'\r';
        if skip {
            continue;
        }
        match byte[0] {
            // Ctrl-D.
            0x04 => return Ok(()),
            b'\r' | b'\n' => {
                out.text("\n").await?;
                if !run(parse(&line), &mut out, device).await? {
                    return Ok(());
                }
                out.text("> ").await?;
                line.clear();
            }
            // Backspace/DEL.
            0x08 | 0x7f if line.pop().is_some() => out.io.write_all(b"\x08 \x08").await?,
            // Длиннее строки — символ не принимается и не отображается.
            c @ 0x20..0x7f if line.push(char::from(c)).is_ok() => out.io.write_all(&byte).await?,
            _ => {}
        }
    }
}

/// Протокол на принятом соединении: вход, сессия и её конец.
async fn session(
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

/// Узел `SSH`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
pub async fn task<K: HostKey>(mut ctx: SshCtx<'_, K>) -> TaskExit {
    ctx.run().await.result
}

// Узел объявлен рядом со своей задачей — как `HTTP`. `<K>` — ключ хоста
// (`Fixed` или `Stored` над хранилищем платы, которого `domain` не видит);
// `local` у него и у стека — оба вправе быть `!Send`. Буферы (~7 KiB) — слоты
// `= static`: через стек они не идут. Без `watchdog:`: сервер ждёт клиента,
// которого может не быть всю жизнь платы.
supervisor::supervisor_fragment! {
    name: SSH_FRAG<K>;
    boot: inputs: $crate::ssh::SshWiring<K>;

    node deps: [], restart: Never,
        resources: [
            /// Сетевой стек, на котором узел открывает свой сокет.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Как пускать пользователя.
            LOGIN: $crate::ssh::Login = inputs.login,
            /// Ключ хоста — собирает его узел из того, что раздал compose-site.
            HOST_KEY: local K = <K as $crate::Assemble>::assemble(inputs.host_key, ()),
            // {%- if ota == "true" %}
            /// Клиент входа `CMD` узла OTA — для `reboot`.
            OTA: $crate::ota::OtaClient = inputs.ota,
            // {%- endif %}
            /// Приём сокета.
            TCP_RX: &'static mut [u8; $crate::ssh::TCP_BUFFER] = static,
            /// Передача сокета.
            TCP_TX: &'static mut [u8; $crate::ssh::TCP_BUFFER] = static,
            /// Пакеты SSH от клиента.
            SSH_RX: &'static mut [u8; $crate::ssh::SSH_BUFFER] = static,
            /// Пакеты SSH клиенту.
            SSH_TX: &'static mut [u8; $crate::ssh::SSH_BUFFER] = static
        ],
        ctx: $crate::ssh::SshCtx,
        task: $crate::ssh::task;
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl user@host";
    // {%- if ssh_host_key == "config" %}

    /// Хранилище в памяти: что записано, то и читается.
    #[derive(Default)]
    struct Memory(Option<Vec<u8>>);

    impl ports::SettingsStorage for Memory {
        type Error = ();

        fn read<'a>(
            &mut self,
            _: u32,
            scratch: &'a mut [u8],
        ) -> impl Future<Output = Result<Option<&'a [u8]>, ()>> {
            // Как у порта: значение длиннее буфера — ошибка, а не обрезка.
            core::future::ready(match &self.0 {
                None => Ok(None),
                Some(v) if v.len() > scratch.len() => Err(()),
                Some(v) => {
                    scratch[..v.len()].copy_from_slice(v);
                    Ok(Some(&scratch[..v.len()]))
                }
            })
        }

        fn write(&mut self, _: u32, value: &[u8]) -> impl Future<Output = Result<(), ()>> {
            self.0 = Some(value.to_vec());
            core::future::ready(Ok(()))
        }
    }
    // {%- endif %}

    #[test]
    fn an_openssh_line_gives_the_raw_key() {
        let key = ed25519_public_key(KEY).expect("ключ разбирается");
        assert_eq!(key[..4], [0xe3, 0x2a, 0xaa, 0x79]);
        assert_eq!(key[28..], [0x7d, 0x18, 0xa2, 0x65]);
        // Комментарий необязателен.
        assert_eq!(
            ed25519_public_key(&KEY[..KEY.len() - " user@host".len()]),
            Some(key)
        );
    }

    #[test]
    fn anything_else_is_not_a_key() {
        for line in [
            "",
            "ssh-ed25519",
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5",
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJ!",
        ] {
            assert_eq!(ed25519_public_key(line), None, "{line}");
        }
    }

    #[test]
    fn commands_are_whole_words() {
        assert_eq!(parse(""), Command::Empty);
        assert_eq!(parse("  help "), Command::Help);
        assert_eq!(parse("info"), Command::Info);
        assert_eq!(parse("exit"), Command::Exit);
        assert_eq!(parse("info now"), Command::Unknown);
        assert_eq!(parse("helpme"), Command::Unknown);
        // {%- if ota == "true" %}
        assert_eq!(parse("reboot"), Command::Reboot { confirmed: false });
        assert_eq!(parse("reboot yes"), Command::Reboot { confirmed: true });
        assert_eq!(parse("reboot yes please"), Command::Unknown);
        // {%- endif %}
    }

    #[test]
    fn info_fits_the_reply_buffer() {
        let info = DeviceInfo {
            version: "65535.65535.65535-rc.65535",
            commit: "0123456789ab",
            chip: "STM32H7R7L8HXH",
            uid: [0xff; crate::device::UID_BYTES],
            flash_size: u32::MAX,
            sys_hz: Some(u32::MAX),
            reset: crate::device::ResetCause::IndependentWatchdog,
            last_panic: Some(String::leak("ж".repeat(PANIC_BRIEF / 2))),
        };
        let mut out = heapless::String::<INFO_TEXT>::new();
        write_info(
            &mut out,
            &info,
            [0xff; 6],
            Some(embassy_net::Ipv4Address::BROADCAST),
            u64::MAX,
        )
        .unwrap();
        assert!(out.starts_with("version 65535"), "{out}");
        assert!(
            out.contains("\nreset independent_watchdog\nlast panic ж"),
            "{out}"
        );
    }
    // {%- if ssh_host_key == "config" %}

    /// Первый старт создаёт ключ и записывает, следующий читает тот же.
    #[tokio::test]
    async fn the_stored_key_survives_a_restart() {
        let mut stored = Stored(Memory::default());
        let first = stored.seed().await;
        assert_ne!(first, [0; 32]);
        assert_eq!(stored.0.0.as_deref(), Some(&first[..]));
        assert_eq!(Stored(Memory(stored.0.0.clone())).seed().await, first);
    }

    /// Ключ не той длины заменяется новым и записывается; раздел, который не
    /// прочитался (здесь — значение длиннее буфера), не перезаписывается:
    /// сбой мог быть временным.
    #[tokio::test]
    async fn only_a_missing_key_is_overwritten() {
        let mut short = Stored(Memory(Some(vec![1; 16])));
        let seed = short.seed().await;
        assert_eq!(short.0.0.as_deref(), Some(&seed[..]));

        let unreadable = vec![2; 64];
        let mut broken = Stored(Memory(Some(unreadable.clone())));
        broken.seed().await;
        assert_eq!(broken.0.0, Some(unreadable));
    }
    // {%- endif %}
}
