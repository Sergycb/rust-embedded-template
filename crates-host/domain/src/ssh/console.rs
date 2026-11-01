//! Команды консоли: разбор строки, выполнение, вывод в канал и интерактивный
//! `shell`.

use defmt_or_log::info;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::{Read, Write};
// {%- if ota == "true" %}
use ports::{OtaCommand, Progress};

use crate::ota::OtaClient;
// {%- endif %}
use crate::device::{DeviceInfo, PANIC_BRIEF};
use crate::ssh::session::CLOSE_WAIT;

/// Длина строки команды.
pub(super) const LINE: usize = 64;

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

/// Что консоли сделать с принятым байтом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    /// Ничего: байт не меняет строку.
    Nothing,
    /// Байт принят в строку — эхо им же.
    Echo(u8),
    /// Последний символ стёрт — стереть его и на экране.
    Erase,
    /// Ctrl-C: строка сброшена.
    Cancel,
    /// Конец строки: выполнить команду.
    Enter(Command),
    /// Ctrl-D на пустой строке: конец сессии.
    Eof,
}

/// Где внутри управляющей последовательности терминала стоит ввод.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Escape {
    /// Вне последовательности.
    #[default]
    Off,
    /// После ESC.
    Started,
    /// Внутри CSI (`ESC [` … финальный байт): стрелки, Home, Delete.
    Csi,
    /// После `ESC O` — SS3, ещё один байт: стрелки и F1–F4 в режиме
    /// приложения.
    Ss3,
}

/// Строка, которую набирает терминал `shell`: байт за байтом, без вывода —
/// что показать, решает вызывающий по [`Key`].
#[derive(Debug, Default)]
pub(super) struct Editor {
    line: heapless::String<LINE>,
    /// Набрано больше [`LINE`]: лишнее не принято, и по Enter строка —
    /// [`Command::Unknown`], а не обрезанный префикс: из длинной строки
    /// иначе собиралась бы другая команда (`reboot` с `yes` на 64-м байте).
    /// Сбрасывают Enter, Ctrl-C и строка, стёртая до пустой; стёртый хвост —
    /// нет: потерянных символов он не возвращает, а начало на экране
    /// осталось от той же длинной строки.
    overflow: bool,
    /// Прошлый байт — `\r`: `\r\n` — один конец строки, а не строка и
    /// пустая за ней.
    after_cr: bool,
    escape: Escape,
}

impl Editor {
    /// Принимает байт от терминала.
    pub(super) fn feed(&mut self, byte: u8) -> Key {
        // Байт, поглощённый последовательностью, — не ввод, и `\r` в нём не
        // склеивается со следующим `\n`.
        let after_cr = core::mem::take(&mut self.after_cr);
        if self.escaped(byte) {
            return Key::Nothing;
        }
        self.after_cr = byte == b'\r';
        match byte {
            0x1b => {
                self.escape = Escape::Started;
                Key::Nothing
            }
            b'\n' if after_cr => Key::Nothing,
            b'\r' | b'\n' => {
                let command = if self.overflow {
                    Command::Unknown
                } else {
                    parse(&self.line)
                };
                self.reset();
                Key::Enter(command)
            }
            // Ctrl-C.
            0x03 => {
                self.reset();
                Key::Cancel
            }
            // Ctrl-D: на набранной строке не обрывает сессию.
            0x04 if self.line.is_empty() => Key::Eof,
            // Backspace/DEL.
            0x08 | 0x7f if self.line.pop().is_some() => {
                if self.line.is_empty() {
                    self.overflow = false;
                }
                Key::Erase
            }
            c @ 0x20..0x7f => {
                if self.line.push(char::from(c)).is_ok() {
                    Key::Echo(c)
                } else {
                    // Не принят и не отображается.
                    self.overflow = true;
                    Key::Nothing
                }
            }
            _ => Key::Nothing,
        }
    }

    /// Поглощает байт управляющей последовательности; `false` — байт —
    /// обычный ввод.
    fn escaped(&mut self, byte: u8) -> bool {
        let (consumed, next) = match (self.escape, byte) {
            // `ESC [`, затем параметры и промежуточные байты CSI.
            (Escape::Started, b'[') | (Escape::Csi, 0x20..=0x3f) => (true, Escape::Csi),
            (Escape::Started, b'O') => (true, Escape::Ss3),
            // Финальный байт CSI и байт за SS3 — конец последовательности.
            (Escape::Csi, 0x40..=0x7e) | (Escape::Ss3, 0x20..=0x7e) => (true, Escape::Off),
            // Вне последовательности; одиночный ESC отброшен, а управляющий
            // байт обрывает начатую — дальше обычный ввод.
            _ => (false, Escape::Off),
        };
        self.escape = next;
        consumed
    }

    /// Новая строка.
    fn reset(&mut self) {
        self.line.clear();
        self.overflow = false;
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

/// Консоль `shell`: строка за строкой до `exit`, Ctrl-D, EOF или
/// [`SHELL_IDLE`] без ввода.
pub(super) async fn shell<IO: Read + Write>(io: IO, device: &Device<'_>) -> Result<(), IO::Error> {
    let mut out = Out { io, crlf: true };
    out.text("console, 'help' for commands\n> ").await?;
    let mut editor = Editor::default();
    let mut byte = [0];
    loop {
        // Брошенное по сроку чтение ничего нужного не теряет: сессия
        // кончается.
        let Ok(read) = with_timeout(SHELL_IDLE, out.io.read(&mut byte)).await else {
            info!(
                "ssh: консоль молчала {} с — сессия закрыта",
                SHELL_IDLE.as_secs()
            );
            // Клиент, переставший читать канал, не держит сессию и
            // прощанием: окно, забитое прежним выводом, ждалось бы вечно.
            let farewell = out.text("\nidle timeout, session closed\n");
            return with_timeout(CLOSE_WAIT, farewell).await.unwrap_or(Ok(()));
        };
        if read? == 0 {
            return Ok(());
        }
        match editor.feed(byte[0]) {
            Key::Nothing => {}
            Key::Echo(c) => out.io.write_all(&[c]).await?,
            Key::Erase => out.io.write_all(b"\x08 \x08").await?,
            Key::Cancel => out.text("^C\n> ").await?,
            Key::Enter(command) => {
                out.text("\n").await?;
                if !run(command, &mut out, device).await? {
                    return Ok(());
                }
                out.text("> ").await?;
            }
            Key::Eof => return Ok(()),
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
