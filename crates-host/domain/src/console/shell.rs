//! Команды консоли: типизированные команды `bareminal`, выполнение, вывод в
//! канал и интерактивный `shell`.
//!
//! Разбор, `help`, историю и автодополнение строит `bareminal`; данные команды
//! берут у своих узлов клиентами их входов{% if ota == "true" %}, как перезагрузку —
//! у OTA{% endif %}. Транспорт — SSH или `cli` — передаёт сюда свой поток:
//! различаются только канал и срок молчания.

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
use defmt_or_log::info;
// {%- if has_net == "true" %}
use embassy_net::Stack;
// {%- endif %}
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::{Read, Write};

// {%- if ota == "true" %}
use crate::ota::Client;
use crate::ota::commands::Progress;
// {%- endif %}
use crate::device::{DeviceInfo, INFO_TEXT, write_info};

/// Длина строки команды и буфера разбора.
pub(crate) const LINE: usize = 128;

/// Сколько символов держит `bareminal` до разбора: та же строка, что у `exec`.
const CMD_BUFFER: usize = LINE;

/// Сколько команд помнит история (Up/Down).
const HISTORY: usize = 4;

// {%- if net_services contains "ssh" %}
/// Буфер текста ошибки разбора `exec`.
const ERROR_TEXT: usize = 64;
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
    pub(crate) ota: Client,
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
            let reply = match device.ota.request(crate::ota::Command::Reboot).await {
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
