//! Команды консоли: разбор строки, выполнение, вывод в канал и интерактивный
//! `shell`.

use embassy_net::Stack;
use embassy_time::Instant;
use embedded_io_async::{Read, Write};
use sunset_async::ChanInOut;
// {%- if ota == "true" %}
use ports::{OtaCommand, Progress};

use crate::ota::OtaClient;
// {%- endif %}
use crate::device::{DeviceInfo, PANIC_BRIEF};

/// Длина строки команды.
pub(super) const LINE: usize = 64;

/// Самый длинный ответ на `info`, байт: 256 — прошлые строки с запасом под
/// длинную версию, 27 — `reset independent_watchdog`, остальное — строка
/// `last panic` с [`PANIC_BRIEF`] байт.
pub(super) const INFO_TEXT: usize = 256 + 27 + "last panic \n".len() + PANIC_BRIEF;

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

/// Команда из строки.
pub(super) fn parse(line: &str) -> Command {
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

/// Выполняет команду; `false` — сессия кончилась (`exit`).
pub(super) async fn run<W: Write>(
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

/// Консоль `shell`: строка за строкой до `exit`, Ctrl-D или EOF.
pub(super) async fn shell(
    io: ChanInOut<'_>,
    device: &Device<'_>,
) -> Result<(), sunset_async::sunset::Error> {
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
    writeln!(out, "reset {}", info.reset.as_str())?;
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
