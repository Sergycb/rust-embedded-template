//! Команды консоли: типизированные команды `bareminal`, выполнение, вывод в
//! канал и интерактивный `shell`.
//!
//! Консоль одна на плату по смыслу, но открывается с того транспорта, который
//! выбран в проекте: SSH (`ssh`) или `cli` поверх UART/USB. Команды, разбор и
//! вход — по паролю, чей хэш вшит в прошивку
//! ([`PasswordHash`](crate::console::PasswordHash)), — общие;
//! различаются только канал и срок молчания.

mod login;

use core::cell::RefCell;
use core::str::FromStr;

// {%- if net_services contains "ssh" %}
use bareminal_cli::buffer::Buffer;
// {%- endif %}
use bareminal_cli::cli::Bareminal;
use bareminal_cli::process::RuntimeError;
// {%- if net_services contains "ssh" %}
use bareminal_cli::process::{CommandsParser, HelpIter, ProcessError};
// {%- endif %}
use bareminal_macros::Command;
// {%- if has_cli == "true" %}
use defmt_or_log::warn;
// {%- endif %}
use defmt_or_log::info;
// {%- if has_net == "true" %}
use embassy_net::Stack;
// {%- endif %}
use embassy_time::{Duration, Instant, with_timeout};
// {%- if has_cli == "true" %}
use embassy_time::Timer;
// {%- endif %}
use embedded_io_async::{Read, Write};

// {%- if ota == "true" %}
use crate::ota::command::Progress;
use crate::ota::{OtaClient, OtaCommand};
// {%- endif %}
use crate::device::{DeviceInfo, PANIC_BRIEF};

pub(crate) use login::Access;
pub use login::{PASSWORD_HASH_LEN, PasswordHash, USER};

// {%- if has_cli == "true" %}
// Узел `CONSOLE`: консоль поверх выбранного UART/USB. Живёт рядом с задачей;
// слот `IO` — `local`, потому что `RefCell` в `shell` делает future `!Send`.
supervisor::supervisor_fragment! {
    name: CONSOLE_FRAG<T>;
    boot: inputs: struct $crate::console::ConsoleWiring;

    node resources: [
            /// Поток консоли: `BufferedUart` или `CdcStream`.
            IO: local T = inputs.io,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Хэш пароля входа.
            LOGIN: $crate::console::PasswordHash = inputs.login,
            // {%- if has_net == "true" %}
            /// Стек сети проекта: `info` показывает MAC и адрес.
            STACK: local $crate::console::NetStack = inputs.stack,
            // {%- endif %}
            // {%- if ota == "true" %}
            /// Клиент входа `CMD` узла OTA — для `reboot`.
            OTA: $crate::ota::OtaClient = inputs.ota,
            // {%- endif %}
        ],
        ctx: $crate::console::ConsoleCtx,
        task: $crate::console::task;
}
// {%- endif %}

/// Длина строки команды и буфера разбора.
pub(crate) const LINE: usize = 128;

/// Сколько символов держит `bareminal` до разбора: та же строка, что у `exec`.
const CMD_BUFFER: usize = LINE;

/// Сколько команд помнит история (Up/Down).
const HISTORY: usize = 4;

// {%- if has_cli == "true" %}
/// Пауза узла `CONSOLE` перед новым приглашением после EOF или ошибки
/// потока: поток, который отказывает сразу, иначе крутил бы цикл без
/// `await` и не отдавал исполнитель другим задачам.
const RESTART_PAUSE: Duration = Duration::from_millis(100);

/// Пауза после неверного пароля: перебор по проводу — не быстрее попытки в
/// секунду.
const WRONG_PASSWORD_PAUSE: Duration = Duration::from_secs(1);
// {%- endif %}

// {%- if net_services contains "ssh" %}
/// Буфер текста ошибки разбора `exec`.
const ERROR_TEXT: usize = 64;
// {%- endif %}

/// Самый длинный ответ на `info`, байт: 256 — прошлые строки с запасом под
/// длинную версию, 27 — `reset independent_watchdog`, остальное — строка
/// `last panic` с [`PANIC_BRIEF`] байт.
pub(crate) const INFO_TEXT: usize = 256 + 27 + "last panic \n".len() + PANIC_BRIEF;

// {%- if has_net == "true" %}
/// Стек консоли. Псевдоним — чтобы `app`, где раскрывается фрагмент, не
/// зависел от `embassy-net` сам.
pub type NetStack = Option<Stack<'static>>;
// {%- endif %}

/// Подтверждение перезагрузки: `reboot` — подсказка, `reboot yes` — сброс.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Без `yes`.
    No,
    /// `reboot yes`.
    Yes,
}

impl FromStr for Confirm {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        match text {
            "no" => Ok(Self::No),
            "yes" => Ok(Self::Yes),
            _ => Err(()),
        }
    }
}

/// Команда консоли.
#[derive(Debug, PartialEq, Command)]
pub enum Command {
    /// Сведения об устройстве: firmware, chip, unique id, MAC, address,
    /// uptime, reset cause, last panic
    Info,
    // {%- if ota == "true" %}
    /// Перезагрузка; без `yes` — только подсказка
    #[set(default = "no", one_of = ["yes", "no"])]
    Reboot(Confirm),
    // {%- endif %}
    /// Конец сессии (как и Ctrl-D)
    Exit,
}

/// Что команды знают об устройстве.
pub(crate) struct Device<'a> {
    pub(crate) info: &'a DeviceInfo,
    // {%- if has_net == "true" %}
    /// Сетевой стек: без него `info` печатает `mac none`, `ip none`.
    pub(crate) stack: Option<Stack<'static>>,
    // {%- endif %}
    // {%- if ota == "true" %}
    pub(crate) ota: OtaClient,
    // {%- endif %}
}

/// Вывод в канал: терминалу (`shell`) строки уходят с `\r\n`, `exec` — как
/// есть.
pub(crate) struct Out<W> {
    pub(crate) io: W,
    pub(crate) crlf: bool,
}

impl<W: Write> Out<W> {
    pub(crate) async fn text(&mut self, text: &str) -> Result<(), W::Error> {
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

/// Как кончилась интерактивная консоль.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellEnd {
    /// `exit` или Ctrl-D.
    Exit,
    /// Канал закрылся.
    Eof,
    /// Консоль молчала дольше [`Idle::after`].
    Idle,
}

/// Срок молчания и прощание — только у SSH: соединение одно, брошенное окно
/// терминала не должно держать его вечно. UART/USB ждут ввод без срока.
#[derive(Debug, Clone, Copy)]
pub struct Idle {
    /// Сколько ждать ввода.
    pub after: Duration,
    /// Что написать клиенту перед закрытием.
    pub farewell: &'static str,
    /// Сколько ждать, пока прощание уйдёт клиенту, который не читает.
    pub write_timeout: Duration,
}

/// Общий доступ к одному `Read + Write` из двух ручек: `Bareminal` владеет
/// пишущей, читает `shell`.
struct Shared<'a, IO>(&'a RefCell<IO>);

impl<IO: Read + Write> embedded_io_async::ErrorType for Shared<'_, IO> {
    type Error = IO::Error;
}

impl<IO: Read + Write> Read for Shared<'_, IO> {
    #[expect(
        clippy::await_holding_refcell_ref,
        reason = "`shell` зовёт чтение и запись по очереди, а брошенное по сроку \
                  чтение отпускает заём раньше прощания"
    )]
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().read(buf).await
    }
}

impl<IO: Read + Write> Write for Shared<'_, IO> {
    #[expect(
        clippy::await_holding_refcell_ref,
        reason = "`shell` зовёт чтение и запись по очереди"
    )]
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().write(buf).await
    }

    #[expect(
        clippy::await_holding_refcell_ref,
        reason = "`shell` зовёт чтение и запись по очереди"
    )]
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().flush().await
    }
}

// {%- if has_cli == "true" %}
#[fsm::typestate]
impl<IO: Read + Write> ConsoleCtx<'_, IO> {
    fsm::machine! {
        machine Console mod edges {
            states {
                /// Консоль ждёт ввод: пароль, затем команды.
                Running { access: Access },
                /// Недостижимо: обслуживание не возвращается. Состояние нужно
                /// таблице — строка без ожидания не может остаться в своём
                /// состоянии.
                exit Down,
            }
            start => begin -> Running;
            transitions {
                Running => serve -> Down,
            }
        }
    }

    fn begin(&mut self) -> Running {
        info!("console: узел запущен");
        Running {
            access: Access::new(*self.login),
        }
    }

    /// Спрашивает пароль, затем ведёт `shell`; после `exit`/Ctrl-D — снова
    /// пароль. Не возвращается.
    async fn serve(&mut self, s: &Running) -> Down {
        loop {
            match sign_in(&mut *self.io, s.access).await {
                Some(true) => info!("console: вход выполнен"),
                Some(false) => {
                    warn!("console: неверный пароль");
                    self.io.write_all(b"wrong password\r\n").await.ok();
                    Timer::after(WRONG_PASSWORD_PAUSE).await;
                    continue;
                }
                None => {
                    Timer::after(RESTART_PAUSE).await;
                    continue;
                }
            }
            let device = Device {
                info: self.info,
                // {%- if has_net == "true" %}
                stack: *self.stack,
                // {%- endif %}
                // {%- if ota == "true" %}
                ota: *self.ota,
                // {%- endif %}
            };
            match shell(&mut *self.io, &device, None).await {
                Ok(ShellEnd::Exit) => info!("console: exit — вход заново"),
                Ok(ShellEnd::Idle) => {}
                Ok(ShellEnd::Eof) => {
                    info!("console: EOF — вход заново");
                    Timer::after(RESTART_PAUSE).await;
                }
                Err(_) => {
                    info!("console: канал оборвался — вход заново");
                    Timer::after(RESTART_PAUSE).await;
                }
            }
        }
    }
}

/// Спрашивает пароль `cli` без эха: `Some(true)` — верный, `Some(false)` —
/// неверный, `None` — поток кончился или отказал.
async fn sign_in<IO: Read + Write>(io: &mut IO, access: Access) -> Option<bool> {
    io.write_all(b"password: ").await.ok()?;
    let mut line = [0; LINE];
    let (mut len, mut long) = (0_usize, false);
    loop {
        let mut byte = [0];
        if io.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        match byte[0] {
            b'\r' | b'\n' if len > 0 || long => break,
            // Пустая строка — хвост `\r\n` прошлого ввода или Enter без
            // пароля: ждём пароль дальше.
            b'\r' | b'\n' => {}
            // Backspace/Delete стирают символ, Ctrl-C/Ctrl-U — строку.
            0x08 | 0x7f => len = len.saturating_sub(1),
            0x03 | 0x15 => (len, long) = (0, false),
            byte => {
                if let Some(slot) = line.get_mut(len) {
                    *slot = byte;
                    len += 1;
                } else {
                    // Длиннее строки — не пароль, даже если начало совпало.
                    long = true;
                }
            }
        }
    }
    io.write_all(b"\r\n").await.ok()?;
    Some(!long && access.admits(&line[..len]))
}
// {%- endif %}

// {%- if net_services contains "ssh" %}
/// Команда из строки для `exec`; ошибка разбора — текстом в `error`.
pub(crate) fn parse(line: &str, error: &mut heapless::String<ERROR_TEXT>) -> Option<Command> {
    let mut buffer = Buffer::<CMD_BUFFER>::from_str(line).ok()?;
    let tokens = buffer.as_tokens()?;
    let mut iter = tokens.iter();
    let result = match Command::parse(&mut iter) {
        // `exec` исполняет одну команду: хвост строки — ошибка, а не
        // следующая команда, как в цепочке `shell`.
        Ok(command) if iter.peek().is_none() => Ok(command),
        Ok(_) => Err(ProcessError::Unknown),
        Err(err) => Err(err),
    };
    match result {
        Ok(command) => Some(command),
        Err(err) => {
            if let Ok(text) = err.to_string::<ERROR_TEXT>() {
                *error = text;
            }
            None
        }
    }
}

/// Печатает `help` в `exec`: у `shell` его рисует `bareminal`, а `exec`
/// обходится без его ANSI-последовательностей.
async fn write_help<W: Write>(out: &mut Out<W>, lines: HelpIter) -> Result<(), W::Error> {
    for line in lines {
        out.text(line.as_ref()).await?;
        out.text("\n").await?;
    }
    Ok(())
}

/// Выполняет одну команду `exec`; вывод — без ANSI и без `\r`.
pub(crate) async fn exec<W: Write>(
    line: &str,
    out: &mut Out<W>,
    device: &Device<'_>,
) -> Result<(), W::Error> {
    let mut words = line.split_ascii_whitespace();
    if words.next() == Some("help") {
        match words.next() {
            Some(name) if words.next().is_none() => {
                let help = Command::help_for(name);
                if help.is_empty() {
                    out.text("Unknown command\n").await?;
                } else {
                    write_help(out, HelpIter::single(help)).await?;
                }
            }
            Some(_) => out.text("Unknown command\n").await?,
            None => write_help(out, Command::help_lines()).await?,
        }
        return Ok(());
    }
    let mut error = heapless::String::<ERROR_TEXT>::new();
    match parse(line, &mut error) {
        Some(command) => {
            run(command, out, device).await?;
        }
        None => {
            if error.is_empty() {
                out.text("unknown command, 'help' for the list\n").await?;
            } else {
                out.text(&error).await?;
                out.text("\n").await?;
            }
        }
    }
    Ok(())
}
// {%- endif %}

/// Выполняет команду; `false` — сессия кончилась (`exit`).
async fn run<W: Write>(
    command: Command,
    out: &mut Out<W>,
    device: &Device<'_>,
) -> Result<bool, W::Error> {
    match command {
        Command::Info => {
            let mut text = heapless::String::<INFO_TEXT>::new();
            // {%- if has_net == "true" %}
            let (mac, ip) = match device.stack {
                // Среда в стеке одна — Ethernet (`medium-ethernet`).
                Some(stack) => {
                    let embassy_net::HardwareAddress::Ethernet(mac) = stack.hardware_address();
                    (
                        Some(mac.0),
                        stack.config_v4().map(|config| config.address.address()),
                    )
                }
                None => (None, None),
            };
            // {%- else %}
            //{{newline}}            let (mac, ip) = (None, None);
            // {%- endif %}
            let uptime_s = Instant::now().as_secs();
            match write_info(&mut text, device.info, mac, ip, uptime_s) {
                Ok(()) => out.text(&text).await?,
                Err(core::fmt::Error) => out.text("info too long\n").await?,
            }
        }
        // {%- if ota == "true" %}
        Command::Reboot(Confirm::No) => out.text("to reboot, type: reboot yes\n").await?,
        Command::Reboot(Confirm::Yes) => {
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
    }
    Ok(true)
}

/// Консоль `shell`: строка за строкой до `exit`, Ctrl-D, EOF или
/// [`Idle::after`] без ввода.
pub(crate) async fn shell<IO: Read + Write>(
    io: IO,
    device: &Device<'_>,
    idle: Option<Idle>,
) -> Result<ShellEnd, RuntimeError> {
    let io = RefCell::new(io);
    {
        let mut tx = Shared(&io);
        tx.write_all(b"console, 'help' for commands\r\n")
            .await
            .map_err(|_err| RuntimeError::WriteFailed)?;
    }
    let mut cli =
        Bareminal::<Command, _, CMD_BUFFER, HISTORY>::with_prompt(Shared(&io), "> ").await?;
    let mut rx = Shared(&io);
    let mut byte = [0];
    loop {
        let read = match idle {
            Some(idle) => {
                // Брошенное по сроку чтение ничего нужного не теряет: сессия
                // кончается.
                let Ok(read) = with_timeout(idle.after, rx.read(&mut byte)).await else {
                    info!(
                        "console: консоль молчала {} с — сессия закрыта",
                        idle.after.as_secs()
                    );
                    // Клиент, переставший читать канал, не держит сессию и
                    // прощанием: окно, забитое прежним выводом, ждалось бы вечно.
                    let farewell = cli.writer().write_all(idle.farewell.as_bytes());
                    return with_timeout(idle.write_timeout, farewell)
                        .await
                        .unwrap_or(Ok(()))
                        .map(|()| ShellEnd::Idle)
                        .map_err(|_err| RuntimeError::WriteFailed);
                };
                read
            }
            None => rx.read(&mut byte).await,
        };
        if read.map_err(|_err| RuntimeError::ReadFailed)? == 0 {
            return Ok(ShellEnd::Eof);
        }
        // Ctrl-D — конец сессии; Ctrl-C в `bareminal` не сбрасывает строку,
        // поэтому он идёт тем же байтом, что и Ctrl-U.
        let byte = match byte[0] {
            0x04 => return Ok(ShellEnd::Exit),
            0x03 => 0x15,
            byte => byte,
        };
        if cli.add_byte(byte).await? {
            loop {
                if let Some((command, writer)) = cli.next_command().await? {
                    let mut out = Out {
                        io: &mut writer.writer,
                        crlf: true,
                    };
                    if !run(command, &mut out, device)
                        .await
                        .map_err(|_err| RuntimeError::WriteFailed)?
                    {
                        return Ok(ShellEnd::Exit);
                    }
                } else {
                    cli.finalize().await?;
                    break;
                }
            }
        }
    }
}

/// Сведения об устройстве текстом.
pub(crate) fn write_info(
    out: &mut impl core::fmt::Write,
    info: &DeviceInfo,
    mac: Option<[u8; 6]>,
    ip: Option<core::net::Ipv4Addr>,
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
    match mac {
        Some(mac) => {
            for (i, b) in mac.iter().enumerate() {
                write!(out, "{}{b:02x}", if i == 0 { "" } else { ":" })?;
            }
            out.write_char('\n')?;
        }
        None => out.write_str("none\n")?,
    }
    match ip {
        Some(ip) => writeln!(out, "ip {ip}")?,
        None => out.write_str("ip none\n")?,
    }
    writeln!(out, "uptime {uptime_s} s")?;
    writeln!(out, "reset {}", info.reset)?;
    // Место и текст паники разделяет её же перевод строки, а консоль держит
    // «ключ значение» по строке: текст идёт через пробел, а не второй строкой
    // без ключа.
    let (place, text) = match info.last_panic {
        Some(panic) => panic.split_once('\n').unwrap_or((panic, "")),
        None => ("none", ""),
    };
    out.write_str("last panic ")?;
    out.write_str(place)?;
    if !text.is_empty() {
        write!(out, " {text}")?;
    }
    out.write_char('\n')
}

// {%- if has_cli == "true" %}
/// Узел `CONSOLE`: обслуживает консоль, пока жива прошивка; не возвращается.
pub async fn task<IO: Read + Write>(mut ctx: ConsoleCtx<'_, IO>) {
    ctx.run().await;
}
// {%- endif %}

#[cfg(test)]
mod tests {
    //! Тесты консоли: разбор, `exec`, `shell`, `info` и срок молчания.

    use core::convert::Infallible;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use embassy_time::{Duration, MockDriver};

    use crate::device::{DeviceInfo, PANIC_BRIEF};

    use super::{Device, INFO_TEXT, LINE, ShellEnd, shell, write_info};
    // {%- if net_services contains "ssh" %}
    use super::{Command, Out, exec, parse};
    // {%- endif %}
    // {%- if net_services contains "ssh" and ota == "true" %}
    use super::Confirm;
    // {%- endif %}
    // {%- if has_cli == "true" %}
    use super::login::tests::hashed;
    use super::{
        Access, ConsoleCtx, PasswordHash, RESTART_PAUSE, WRONG_PASSWORD_PAUSE, sign_in, task,
    };

    /// Приветствие `shell`: сколько их — столько раз открывался `shell`.
    const BANNER: &str = "console, 'help' for commands";

    /// Запрос пароля узла `CONSOLE`.
    const PROMPT: &str = "password: ";
    // {%- endif %}

    /// Терминал в памяти: отдаёт `input` по байту, запоминает вывод.
    struct Terminal {
        input: &'static [u8],
        shown: Vec<u8>,
        room: usize,
    }

    impl embedded_io_async::ErrorType for Terminal {
        type Error = Infallible;
    }

    impl embedded_io_async::Read for Terminal {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "сигнатуру задаёт трейт `Read`; сценарий в памяти не ждёт"
        )]
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Infallible> {
            let Some((&byte, rest)) = self.input.split_first() else {
                return Ok(0);
            };
            buf[0] = byte;
            self.input = rest;
            Ok(1)
        }
    }

    impl embedded_io_async::Write for Terminal {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
            if self.room == 0 {
                core::future::pending::<()>().await;
            }
            let n = buf.len().min(self.room);
            self.shown.extend_from_slice(&buf[..n]);
            self.room -= n;
            Ok(n)
        }

        #[expect(
            clippy::unused_async_trait_impl,
            reason = "сигнатуру задаёт трейт `Write`; у памяти нечего досылать"
        )]
        async fn flush(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }

    /// Терминал, который молчит: чтение не кончается, вывод копится.
    struct Silent {
        shown: Vec<u8>,
        room: usize,
    }

    impl embedded_io_async::ErrorType for Silent {
        type Error = Infallible;
    }

    impl embedded_io_async::Read for Silent {
        async fn read(&mut self, _: &mut [u8]) -> Result<usize, Infallible> {
            core::future::pending().await
        }
    }

    impl embedded_io_async::Write for Silent {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
            if self.room == 0 {
                core::future::pending::<()>().await;
            }
            let n = buf.len().min(self.room);
            self.shown.extend_from_slice(&buf[..n]);
            self.room -= n;
            Ok(n)
        }

        #[expect(
            clippy::unused_async_trait_impl,
            reason = "сигнатуру задаёт трейт `Write`; у памяти нечего досылать"
        )]
        async fn flush(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }

    // {%- if has_cli == "true" %}
    /// Порт, в который печатают `input`, а дальше молчат: поток UART/USB
    /// не кончается.
    struct Keyboard {
        input: &'static [u8],
        shown: Vec<u8>,
    }

    impl embedded_io_async::ErrorType for Keyboard {
        type Error = Infallible;
    }

    impl embedded_io_async::Read for Keyboard {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Infallible> {
            let Some((&byte, rest)) = self.input.split_first() else {
                return core::future::pending().await;
            };
            buf[0] = byte;
            self.input = rest;
            Ok(1)
        }
    }

    impl embedded_io_async::Write for Keyboard {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "сигнатуру задаёт трейт `Write`; память не ждёт"
        )]
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
            self.shown.extend_from_slice(buf);
            Ok(buf.len())
        }

        #[expect(
            clippy::unused_async_trait_impl,
            reason = "сигнатуру задаёт трейт `Write`; у памяти нечего досылать"
        )]
        async fn flush(&mut self) -> Result<(), Infallible> {
            Ok(())
        }
    }
    // {%- endif %}

    /// Сведения об устройстве для консоли — значения не важны.
    fn device_info() -> DeviceInfo {
        DeviceInfo {
            version: "0.1.0",
            commit: "abc",
            chip: "STM32G071RB",
            uid: [0; crate::device::UID_BYTES],
            flash_size: 0,
            sys_hz: None,
            reset: "software",
            last_panic: None,
        }
    }

    fn device(info: &DeviceInfo) -> Device<'_> {
        Device {
            info,
            // {%- if has_net == "true" %}
            stack: None,
            // {%- endif %}
            // {%- if ota == "true" %}
            ota: crate::ota::tests::cmd().0,
            // {%- endif %}
        }
    }

    // {%- if net_services contains "ssh" %}
    /// Разбор команд — тем же `Command::parse`, что у `shell` и `exec`.
    fn command(line: &str) -> Option<Command> {
        let mut error = heapless::String::<64>::new();
        parse(line, &mut error)
    }

    #[test]
    fn commands_are_typed_and_qualified() {
        assert_eq!(command("info"), Some(Command::Info));
        assert_eq!(command("exit"), Some(Command::Exit));
        assert_eq!(command("info now"), None);
        assert_eq!(command("helpme"), None);
        assert_eq!(command(""), None);
        // {%- if ota == "true" %}
        assert_eq!(command("reboot"), Some(Command::Reboot(Confirm::No)));
        assert_eq!(command("reboot yes"), Some(Command::Reboot(Confirm::Yes)));
        assert_eq!(command("reboot no"), Some(Command::Reboot(Confirm::No)));
        assert_eq!(command("reboot yes please"), None);
        assert_eq!(command("reboot maybe"), None);
        // {%- endif %}
    }
    // {%- endif %}

    /// Консоль исполняет команды из enum: `help` встроен, неизвестная строка
    /// даёт ошибку, Ctrl-C сбрасывает набранное.
    #[test]
    fn the_shell_runs_derived_commands() {
        const BANNER: &str = "console, 'help' for commands\r\n";
        let info = device_info();
        let device = device(&info);
        let mut cx = Context::from_waker(Waker::noop());
        for (input, expected) in [
            (&b"info\r"[..], "version 0.1.0"),
            (b"help\r", "reboot"),
            (b"unknown\r", "Unknown"),
            // Ctrl-C сбрасывает `exi`, дальше идёт `info`.
            (b"exi\x03info\r", "version 0.1.0"),
        ] {
            let mut terminal = Terminal {
                input,
                shown: Vec::new(),
                room: usize::MAX,
            };
            {
                let console = pin!(shell(&mut terminal, &device, None)).poll(&mut cx);
                assert_eq!(
                    console,
                    Poll::Ready(Ok(ShellEnd::Eof)),
                    "{}",
                    input.escape_ascii()
                );
            }
            let shown = String::from_utf8(terminal.shown).expect("вывод — UTF-8");
            assert!(
                shown.starts_with(BANNER),
                "{}: {shown:?}",
                input.escape_ascii()
            );
            assert!(
                shown.contains(expected),
                "{}: {shown:?}",
                input.escape_ascii()
            );
        }
    }

    /// Ctrl-D закрывает сессию сразу, на любой строке; непрочитанный ввод
    /// остаётся в канале.
    #[test]
    fn ctrl_d_closes_the_shell() {
        let info = device_info();
        let device = device(&info);
        let mut cx = Context::from_waker(Waker::noop());
        for input in [&b"\x04info\r"[..], b"exi\x04info\r"] {
            let mut terminal = Terminal {
                input,
                shown: Vec::new(),
                room: usize::MAX,
            };
            let console = pin!(shell(&mut terminal, &device, None)).poll(&mut cx);
            assert_eq!(
                console,
                Poll::Ready(Ok(ShellEnd::Exit)),
                "{}",
                input.escape_ascii()
            );
            assert_eq!(terminal.input, b"info\r", "{}", input.escape_ascii());
        }
    }

    /// Строка длиннее [`LINE`] не роняет сессию и не превращается в другую
    /// команду: разбор видит усечённую строку.
    #[test]
    fn an_overlong_line_is_not_a_command() {
        let mut input = vec![b'x'; LINE + 32];
        input.push(b'\r');
        let input: &'static [u8] = input.leak();
        let info = device_info();
        let device = device(&info);
        let mut terminal = Terminal {
            input,
            shown: Vec::new(),
            room: usize::MAX,
        };
        let mut cx = Context::from_waker(Waker::noop());
        let console = pin!(shell(&mut terminal, &device, None)).poll(&mut cx);
        assert_eq!(console, Poll::Ready(Ok(ShellEnd::Eof)));
        let shown = String::from_utf8(terminal.shown).expect("вывод — UTF-8");
        assert!(shown.contains("Unknown"), "{shown:?}");
    }

    // {%- if net_services contains "ssh" %}
    /// `exec` не печатает ни ANSI, ни `\r`: его вывод читают скрипты.
    #[test]
    fn exec_writes_plain_text() {
        let info = device_info();
        let device = device(&info);
        let mut cx = Context::from_waker(Waker::noop());
        for (input, expected) in [
            ("info", "version 0.1.0"),
            ("help", "reboot"),
            ("nope", "Unknown"),
        ] {
            let mut terminal = Terminal {
                input: b"",
                shown: Vec::new(),
                room: usize::MAX,
            };
            {
                let mut out = Out {
                    io: &mut terminal,
                    crlf: false,
                };
                let done = pin!(exec(input, &mut out, &device)).poll(&mut cx);
                assert_eq!(done, Poll::Ready(Ok(())), "{input}");
            }
            assert!(
                terminal
                    .shown
                    .windows(expected.len())
                    .any(|w| w == expected.as_bytes()),
                "{input}: {:?}",
                String::from_utf8_lossy(&terminal.shown)
            );
            assert!(!terminal.shown.contains(&b'\r'), "{input}");
            assert!(!terminal.shown.contains(&0x1b), "{input}");
        }
    }
    // {%- endif %}

    /// Консоль без ввода закрывается через [`Idle`], не раньше, и говорит
    /// клиенту почему; клиент, который не читает, прощание держит не дольше
    /// `write_timeout`.
    #[test]
    fn a_silent_shell_closes_after_idle() {
        const PROMPT: &str = "console, 'help' for commands\r\n> ";
        const FAREWELL: &str = "\r\nidle timeout, session closed\r\n";
        const AFTER: Duration = Duration::from_secs(15 * 60);
        const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
        let info = device_info();
        let device = device(&info);
        let mut cx = Context::from_waker(Waker::noop());
        MockDriver::get().reset();

        let mut terminal = Silent {
            shown: Vec::new(),
            room: usize::MAX,
        };
        {
            let idle = Some(super::Idle {
                after: AFTER,
                farewell: FAREWELL,
                write_timeout: WRITE_TIMEOUT,
            });
            let mut console = pin!(shell(&mut terminal, &device, idle));
            assert!(console.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(AFTER - Duration::from_secs(1));
            assert!(console.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(Duration::from_secs(1));
            assert_eq!(
                console.as_mut().poll(&mut cx),
                Poll::Ready(Ok(ShellEnd::Idle))
            );
        }
        assert_eq!(terminal.shown, format!("{PROMPT}{FAREWELL}").as_bytes());

        let mut terminal = Silent {
            shown: Vec::new(),
            room: PROMPT.len(),
        };
        let idle = Some(super::Idle {
            after: AFTER,
            farewell: FAREWELL,
            write_timeout: WRITE_TIMEOUT,
        });
        let mut console = pin!(shell(&mut terminal, &device, idle));
        assert!(console.as_mut().poll(&mut cx).is_pending());
        MockDriver::get().advance(AFTER);
        assert!(console.as_mut().poll(&mut cx).is_pending());
        MockDriver::get().advance(WRITE_TIMEOUT);
        assert_eq!(
            console.as_mut().poll(&mut cx),
            Poll::Ready(Ok(ShellEnd::Idle))
        );
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
            reset: "independent_watchdog",
            last_panic: Some(String::leak("ж".repeat(PANIC_BRIEF / 2))),
        };
        let mut out = heapless::String::<INFO_TEXT>::new();
        write_info(
            &mut out,
            &info,
            Some([0xff; 6]),
            Some(core::net::Ipv4Addr::BROADCAST),
            u64::MAX,
        )
        .unwrap();
        assert!(out.starts_with("version 65535"), "{out}");
        assert!(
            out.contains("\nreset independent_watchdog\nlast panic ж"),
            "{out}"
        );
    }

    /// Место и текст паники — одна строка `last panic …`, а не две.
    #[test]
    fn the_panic_stays_on_its_line() {
        let info = DeviceInfo {
            version: "0.1.0",
            commit: "abc",
            chip: "STM32G071RB",
            uid: [0; crate::device::UID_BYTES],
            flash_size: 0,
            sys_hz: None,
            reset: "software",
            last_panic: Some("dhcp.rs:12:5:\nlease lost"),
        };
        let mut out = heapless::String::<INFO_TEXT>::new();
        write_info(&mut out, &info, None, None, 0).unwrap();
        assert!(
            out.ends_with("\nlast panic dhcp.rs:12:5: lease lost\n"),
            "{out}"
        );
    }

    // {%- if has_cli == "true" %}
    /// Сколько раз в выводе `shown` встретилось `what`.
    fn count(shown: &[u8], what: &str) -> usize {
        shown
            .windows(what.len())
            .filter(|w| *w == what.as_bytes())
            .count()
    }

    /// Узел `CONSOLE` над `io` с паролем `s3cret`: опрос, затем по опросу
    /// после каждого сдвига часов из `advances`. Узел ни разу не кончается.
    fn poll_node<IO: embedded_io_async::Read + embedded_io_async::Write>(
        io: &mut IO,
        advances: &[Duration],
    ) {
        MockDriver::get().reset();
        let mut info = device_info();
        let mut login = PasswordHash(hashed("s3cret"));
        // {%- if has_net == "true" %}
        let mut stack = None;
        // {%- endif %}
        // {%- if ota == "true" %}
        let mut ota = crate::ota::tests::cmd().0;
        // {%- endif %}
        let mut cx = Context::from_waker(Waker::noop());
        let mut node = pin!(task(ConsoleCtx {
            io,
            info: &mut info,
            login: &mut login,
            // {%- if has_net == "true" %}
            stack: &mut stack,
            // {%- endif %}
            // {%- if ota == "true" %}
            ota: &mut ota,
            // {%- endif %}
            index: 0,
        }));
        assert!(node.as_mut().poll(&mut cx).is_pending());
        for &step in advances {
            MockDriver::get().advance(step);
            assert!(node.as_mut().poll(&mut cx).is_pending());
        }
    }

    /// `shell` — только после пароля; `exit` возвращает к паролю, а не роняет
    /// узел.
    #[test]
    fn the_node_asks_the_password_again_after_exit() {
        let mut io = Keyboard {
            input: b"s3cret\rexit\r",
            shown: Vec::new(),
        };
        poll_node(&mut io, &[]);
        let shown = String::from_utf8_lossy(&io.shown);
        assert_eq!(count(&io.shown, BANNER), 1, "{shown:?}");
        assert_eq!(count(&io.shown, PROMPT), 2, "{shown:?}");
    }

    /// Неверный пароль — отказ и пауза [`WRONG_PASSWORD_PAUSE`] до нового
    /// приглашения; `shell` не открывается.
    #[test]
    fn a_wrong_password_is_refused_with_a_pause() {
        for (advances, prompts, banners) in [
            (&[][..], 1, 0),
            (&[WRONG_PASSWORD_PAUSE - Duration::from_millis(1)][..], 1, 0),
            (&[WRONG_PASSWORD_PAUSE][..], 2, 1),
        ] {
            let mut io = Keyboard {
                input: b"nope\rs3cret\r",
                shown: Vec::new(),
            };
            poll_node(&mut io, advances);
            let shown = String::from_utf8_lossy(&io.shown);
            assert_eq!(count(&io.shown, "wrong password"), 1, "{shown:?}");
            assert_eq!(count(&io.shown, PROMPT), prompts, "{advances:?}: {shown:?}");
            assert_eq!(count(&io.shown, BANNER), banners, "{advances:?}: {shown:?}");
        }
    }

    /// Поток, который кончается сразу, не крутит узел без `await`: новое
    /// приглашение — не раньше [`RESTART_PAUSE`].
    #[test]
    fn a_dead_stream_does_not_spin_the_node() {
        for (advances, prompts) in [
            (&[RESTART_PAUSE - Duration::from_millis(1)][..], 1),
            (&[RESTART_PAUSE][..], 2),
        ] {
            let mut io = Terminal {
                input: b"",
                shown: Vec::new(),
                room: usize::MAX,
            };
            poll_node(&mut io, advances);
            assert_eq!(count(&io.shown, PROMPT), prompts, "{advances:?}");
        }
    }

    /// Пароль читается без эха; пустые строки (хвост `\r\n`) пропускаются,
    /// Backspace стирает символ, Ctrl-U — строку, а строка длиннее [`LINE`] —
    /// не пароль, даже если начало совпало.
    #[test]
    fn the_password_line_is_read_without_echo() {
        let access = Access::new(PasswordHash(hashed("s3cret")));
        let long: &'static [u8] = [b"s3cret".as_slice(), &[b'x'; LINE], b"\r"].concat().leak();
        let mut cx = Context::from_waker(Waker::noop());
        for (input, admitted) in [
            (&b"s3cret\r"[..], true),
            (b"\n\r\ns3cret\n", true),
            (b"s3crex\x08t\r", true),
            (b"junk\x15s3cret\r", true),
            (b"s3cret!\r", false),
            (long, false),
        ] {
            let mut terminal = Terminal {
                input,
                shown: Vec::new(),
                room: usize::MAX,
            };
            let read = pin!(sign_in(&mut terminal, access)).poll(&mut cx);
            assert_eq!(
                read,
                Poll::Ready(Some(admitted)),
                "{}",
                input.escape_ascii()
            );
            assert_eq!(
                terminal.shown,
                b"password: \r\n",
                "{}",
                input.escape_ascii()
            );
        }
    }
    // {%- endif %}
}
