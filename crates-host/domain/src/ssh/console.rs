//! Команды консоли: типизированные команды `bareminal`, выполнение, вывод в
//! канал и интерактивный `shell`.

use core::str::FromStr;

use bareminal_cli::buffer::Buffer;
use bareminal_cli::cli::Bareminal;
use bareminal_cli::process::{CommandsParser, HelpIter, ProcessError, RuntimeError};
use bareminal_macros::Command;
use defmt_or_log::info;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::{Read, Write};

// {%- if ota == "true" %}
use crate::ota::command::Progress;
use crate::ota::{OtaClient, OtaCommand};
// {%- endif %}
use crate::device::{DeviceInfo, PANIC_BRIEF};
use crate::ssh::session::CLOSE_WAIT;

/// Длина строки команды и буфера разбора.
pub(super) const LINE: usize = 128;

/// Сколько символов держит `bareminal` до разбора: та же строка, что у `exec`.
const CMD_BUFFER: usize = LINE;

/// Сколько команд помнит история (Up/Down).
const HISTORY: usize = 4;

/// Буфер текста ошибки разбора.
const ERROR_TEXT: usize = 64;

/// Сколько `shell` ждёт ввода, прежде чем закрыть сессию. Соединение у узла
/// одно, а живой, но молчащий клиент (забытое окно терминала) держал бы его
/// бессрочно: пробы keep-alive он подтверждает, и `IDLE` сокета не
/// срабатывает, а keep-alive самого SSH до консоли не доходит. 15 минут —
/// как `TMOUT=900` в рекомендациях CIS для оболочки: паузу, пока человек
/// читает или отвлёкся, это переживает, брошенную консоль — отпускает.
pub(super) const SHELL_IDLE: Duration = Duration::from_secs(15 * 60);

/// Самый длинный ответ на `info`, байт: 256 — прошлые строки с запасом под
/// длинную версию, 27 — `reset independent_watchdog`, остальное — строка
/// `last panic` с [`PANIC_BRIEF`] байт.
pub(super) const INFO_TEXT: usize = 256 + 27 + "last panic \n".len() + PANIC_BRIEF;

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
pub(super) struct Device<'a> {
    pub(super) info: &'a DeviceInfo,
    pub(super) stack: Stack<'static>,
    // {%- if ota == "true" %}
    pub(super) ota: OtaClient,
    // {%- endif %}
}

/// Вывод в канал: терминалу (`shell`) строки уходят с `\r\n`, `exec` — как
/// есть.
pub(super) struct Out<W> {
    pub(super) io: W,
    pub(super) crlf: bool,
}

impl<W: Write> Out<W> {
    pub(super) async fn text(&mut self, text: &str) -> Result<(), W::Error> {
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

/// Команда из строки для `exec`; ошибка разбора — текстом в `error`.
pub(super) fn parse(line: &str, error: &mut heapless::String<ERROR_TEXT>) -> Option<Command> {
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
pub(super) async fn exec<W: Write>(
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

/// Выполняет команду; `false` — сессия кончилась (`exit`).
async fn run<W: Write>(
    command: Command,
    out: &mut Out<W>,
    device: &Device<'_>,
) -> Result<bool, W::Error> {
    match command {
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
/// [`SHELL_IDLE`] без ввода.
pub(super) async fn shell<R: Read, W: Write + Unpin>(
    mut rx: R,
    mut tx: W,
    device: &Device<'_>,
) -> Result<(), RuntimeError> {
    tx.write_all(b"console, 'help' for commands\r\n")
        .await
        .map_err(|_err| RuntimeError::WriteFailed)?;
    let mut cli = Bareminal::<Command, _, CMD_BUFFER, HISTORY>::with_prompt(tx, "> ").await?;
    let mut byte = [0];
    loop {
        // Брошенное по сроку чтение ничего нужного не теряет: сессия
        // кончается.
        let Ok(read) = with_timeout(SHELL_IDLE, rx.read(&mut byte)).await else {
            info!(
                "ssh: консоль молчала {} с — сессия закрыта",
                SHELL_IDLE.as_secs()
            );
            // Клиент, переставший читать канал, не держит сессию и
            // прощанием: окно, забитое прежним выводом, ждалось бы вечно.
            let farewell = cli
                .writer()
                .write_all(b"\r\nidle timeout, session closed\r\n");
            return with_timeout(CLOSE_WAIT, farewell)
                .await
                .unwrap_or(Ok(()))
                .map_err(|_err| RuntimeError::WriteFailed);
        };
        if read.map_err(|_err| RuntimeError::ReadFailed)? == 0 {
            return Ok(());
        }
        // Ctrl-D — конец сессии; Ctrl-C в `bareminal` не сбрасывает строку,
        // поэтому он идёт тем же байтом, что и Ctrl-U.
        let byte = match byte[0] {
            0x04 => return Ok(()),
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
                        return Ok(());
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
pub(super) fn write_info(
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
