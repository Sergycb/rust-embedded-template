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
//! неудач и 30 секунд на вход на соединение. Консоль без ввода закрывается через
//! 15 минут: соединение у узла одно.
//!
//! Чего в `sunset` 0.6 нет: `exit-status` (OpenSSH выходит с кодом 255) и
//! EOF/CLOSE канала по инициативе сервера — оба `sunset` шлёт только эхом на
//! пакеты клиента. Поэтому соединение после сессии закрывает узел, а вывод
//! `exec` встаёт в очередь раньше, чем разбирается EOF клиента (см.
//! `session`).

mod console;
mod host_key;
mod login;
mod session;

use defmt_or_log::{info, warn};
use embassy_net::tcp::TcpSocket;
use embassy_time::with_timeout;
use sunset_async::SSHServer;
use sunset_async::sunset::SignKey;
use sunset_async::sunset::ed25519_dalek::SigningKey;

use crate::ssh::console::Device;
use crate::ssh::login::Access;
use crate::ssh::session::{CLOSE_WAIT, IDLE, KEEP_ALIVE, session};

pub use console::Command;
pub use host_key::{Fixed, HostKey};
// {%- if ssh_host_key == "config" %}
pub use host_key::{HOST_KEY_ID, Stored};
// {%- endif %}
pub use login::{Login, USER, ed25519_public_key};
pub use session::{PORT, SSH_BUFFER, TCP_BUFFER};

// Узел объявлен рядом со своей задачей — как `HTTP`. `<K>` — ключ хоста
// (`Fixed` или `Stored` над хранилищем платы, которого `domain` не видит);
// `local` у него и у стека — оба вправе быть `!Send`. Буферы (~7 KiB) — слоты
// `= static`: через стек они не идут. Без `watchdog:`: сервер ждёт клиента,
// которого может не быть всю жизнь платы.
supervisor::supervisor_fragment! {
    name: SSH_FRAG<K>;
    boot: inputs: struct $crate::ssh::SshWiring;

    node resources: [
            /// Сетевой стек, на котором узел открывает свой сокет.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Как пускать пользователя.
            LOGIN: $crate::ssh::Login = inputs.login,
            /// Ключ хоста.
            HOST_KEY: local K = inputs.host_key,
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

#[fsm::typestate]
impl<K: HostKey> SshCtx<'_, K> {
    fsm::machine! {
        machine Ssh mod edges {
            states {
                /// Ждёт соединения: ключ хоста и кого пускать.
                Listening { key: SignKey, access: Access },
                /// Недостижимо: обслуживание не возвращается. Состояние нужно
                /// таблице — строка без ожидания не может остаться в своём состоянии.
                exit Down,
            }
            start => begin -> Listening;
            transitions {
                Listening => serve -> Down,
            }
        }
    }

    async fn begin(&mut self) -> Listening {
        info!("ssh: узел запущен");
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

/// Узел `SSH`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
pub async fn task<K: HostKey>(mut ctx: SshCtx<'_, K>) {
    ctx.run().await;
}

#[cfg(test)]
mod tests {
    //! Тесты узла `SSH`: ключ пользователя, вход, команды, `info`, ключ хоста.

    use core::convert::Infallible;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use embassy_time::{Duration, MockDriver};

    use crate::device::{DeviceInfo, PANIC_BRIEF};
    use crate::ssh::console::{
        Device, Editor, INFO_TEXT, Key, LINE, SHELL_IDLE, parse, shell, write_info,
    };
    use crate::ssh::login::Access;
    use crate::ssh::session::{CLOSE_WAIT, tries_exhausted};
    use crate::ssh::{Command, Login, USER, ed25519_public_key};
    // {%- if ssh_host_key == "config" %}
    use crate::ssh::{HostKey, Stored};
    // {%- endif %}

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl user@host";
    // {%- if ssh_host_key == "config" %}

    /// Хранилище в памяти: что записано, то и читается.
    #[derive(Default)]
    struct Memory(Option<Vec<u8>>);

    impl ports::SettingsStorage for Memory {
        type Error = ();
        const MAX_VALUE: usize = usize::MAX;

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

    /// Терминал, который молчит: чтение не кончается, вывод копится, пока в
    /// окне канала есть место (`room` байт), дальше запись ждёт вечно.
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

    /// Терминал по сценарию: отдаёт байты `input` по одному, затем EOF;
    /// вывод копится.
    struct Script {
        input: &'static [u8],
        shown: Vec<u8>,
    }

    impl embedded_io_async::ErrorType for Script {
        type Error = Infallible;
    }

    impl embedded_io_async::Read for Script {
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

    impl embedded_io_async::Write for Script {
        #[expect(
            clippy::unused_async_trait_impl,
            reason = "сигнатуру задаёт трейт `Write`; память пишется сразу"
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
            // Блоб нужной длины (51 байт), но имя типа внутри — не ed25519.
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE4AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
            // Блоб нужной длины, но длина ключа внутри — 31, а не 32.
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAH+MqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
            // Верный блоб и ещё три байта — base64 длиннее буфера.
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJlAAAA",
        ] {
            assert_eq!(ed25519_public_key(line), None, "{line}");
        }
    }

    /// По паролю пускает только пароль прошивки вместе с [`USER`]; при входе
    /// по ключу (и при ключе, который не разобрался) пароль не пускает никого.
    #[test]
    fn a_password_lets_in_only_its_user() {
        let by_password = Access::new(Login::Password("s3cret"));
        // Сверка `sunset` здесь — простое равенство: решение от неё не
        // зависит.
        for (access, user, password, admitted) in [
            (by_password, USER, "s3cret", true),
            (by_password, USER, "s3cre", false),
            (by_password, USER, "S3CRET", false),
            (by_password, USER, "", false),
            (by_password, "root", "s3cret", false),
            // Вход по ключу, а пришёл пароль.
            (Access::new(Login::Key(KEY)), USER, "s3cret", false),
            (Access::new(Login::Key("мусор")), USER, "s3cret", false),
            (Access::new(Login::Key("мусор")), USER, "", false),
        ] {
            assert_eq!(
                access.admits_password(|u| u == user, |p| p == password),
                admitted,
                "{user} {password}"
            );
        }
    }

    /// Пустой пароль прошивки — не «пускать всех»: ни пустой пароль, ни
    /// какой-либо другой, ни ключ не проходят.
    #[test]
    fn an_empty_password_lets_no_one_in() {
        let empty = Access::new(Login::Password(""));
        for password in ["", "s3cret"] {
            assert!(
                !empty.admits_password(|u| u == USER, |p| p == password),
                "{password:?}"
            );
        }
        let key = ed25519_public_key(KEY).expect("ключ разбирается");
        assert_eq!(
            empty.admits_key(Some(key), |_| Ok::<_, ()>(true)),
            Ok(false)
        );
    }

    /// По ключу пускает только ключ прошивки вместе с [`USER`]; при входе по
    /// паролю (и при ключе прошивки, который не разобрался) ключ не пускает
    /// никого.
    #[test]
    fn a_key_lets_in_only_its_user() {
        let key = ed25519_public_key(KEY).expect("ключ разбирается");
        let by_key = Access::new(Login::Key(KEY));
        for (access, user, given, admitted) in [
            (by_key, USER, Some(key), true),
            (by_key, "root", Some(key), false),
            (by_key, USER, Some([0x55; 32]), false),
            // Ключ не ed25519.
            (by_key, USER, None, false),
            // Вход по паролю, а пришёл ключ.
            (
                Access::new(Login::Password("s3cret")),
                USER,
                Some(key),
                false,
            ),
            (Access::new(Login::Key("мусор")), USER, Some(key), false),
            (Access::new(Login::Key("мусор")), USER, Some([0; 32]), false),
        ] {
            assert_eq!(
                access.admits_key(given, |u| Ok::<_, ()>(u == user)),
                Ok(admitted),
                "{user} {given:?}"
            );
        }
        // Чужой ключ — неудача, а имя не спрашивается: ошибка его разбора
        // закрыла бы соединение, а не засчиталась попыткой.
        assert_eq!(by_key.admits_key(Some([0x55; 32]), |_| Err(())), Ok(false));
    }

    /// Пять неудач — ещё попытка, шестая закрывает соединение.
    #[test]
    fn the_sixth_failure_closes_the_connection() {
        for failures in 0..6 {
            assert!(!tries_exhausted(failures), "{failures}");
        }
        assert!(tries_exhausted(6));
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

    /// Что консоль делает с байтами `input`, без [`Key::Nothing`].
    fn keys(editor: &mut Editor, input: &[u8]) -> Vec<Key> {
        input
            .iter()
            .map(|&byte| editor.feed(byte))
            .filter(|key| *key != Key::Nothing)
            .collect()
    }

    /// Эхо каждого байта `text`.
    fn echo(text: &str) -> impl Iterator<Item = Key> {
        text.bytes().map(Key::Echo)
    }

    /// Правка строки: переводы строки, Backspace, управляющие
    /// последовательности стрелок, Ctrl-C и Ctrl-D.
    #[test]
    fn the_editor_takes_only_what_it_shows() {
        let cases: [(&[u8], Vec<Key>); 15] = [
            (
                b"help\r",
                echo("help").chain([Key::Enter(Command::Help)]).collect(),
            ),
            // `\r\n` — один конец строки; `\r` и `\n` поодиночке — каждый.
            (
                b"help\r\n",
                echo("help").chain([Key::Enter(Command::Help)]).collect(),
            ),
            (b"\r\r", vec![Key::Enter(Command::Empty); 2]),
            (b"\n\n", vec![Key::Enter(Command::Empty); 2]),
            (
                b"inf\x08xy\x7f\x7fo\r",
                echo("inf")
                    .chain([Key::Erase])
                    .chain(echo("xy"))
                    .chain([Key::Erase; 2])
                    .chain(echo("o"))
                    .chain([Key::Enter(Command::Unknown)])
                    .collect(),
            ),
            // Backspace на пустой строке не стирает приглашение.
            (b"\x7f\r", vec![Key::Enter(Command::Empty)]),
            // Стрелки и Ctrl-стрелки (CSI с параметрами) — ни в строку, ни
            // в эхо.
            (
                b"\x1b[A\x1b[1;5Cinfo\x1b[D\r",
                echo("info").chain([Key::Enter(Command::Info)]).collect(),
            ),
            // SS3 — стрелка в режиме приложения.
            (
                b"\x1bOAinfo\r",
                echo("info").chain([Key::Enter(Command::Info)]).collect(),
            ),
            // Одиночный ESC отброшен, следующий байт — обычный ввод.
            (
                b"\x1binfo\r",
                echo("info").chain([Key::Enter(Command::Info)]).collect(),
            ),
            // Оборванные CSI и SS3: перевод строки внутри — конец строки,
            // и `\r\n` — всё так же один.
            (
                b"info\x1b[1\r\n",
                echo("info").chain([Key::Enter(Command::Info)]).collect(),
            ),
            (
                b"info\x1bO\r\n",
                echo("info").chain([Key::Enter(Command::Info)]).collect(),
            ),
            // Ctrl-C сбрасывает набранное.
            (
                b"exi\x03info\r",
                echo("exi")
                    .chain([Key::Cancel])
                    .chain(echo("info"))
                    .chain([Key::Enter(Command::Info)])
                    .collect(),
            ),
            (b"\x04", vec![Key::Eof]),
            // Ctrl-D на набранной строке сессию не обрывает.
            (
                b"exit\x04\r",
                echo("exit").chain([Key::Enter(Command::Exit)]).collect(),
            ),
            // Прочие управляющие байты не попадают ни в строку, ни в эхо.
            (
                b"in\tfo\x00\r",
                echo("info").chain([Key::Enter(Command::Info)]).collect(),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                keys(&mut Editor::default(), input),
                expected,
                "{}",
                input.escape_ascii()
            );
        }
    }

    /// Строка длиннее [`LINE`] — неизвестная команда, а не префикс: из
    /// `reboot`, пробелов и `yes` на последних байтах собралась бы другая.
    #[test]
    fn an_overlong_line_is_no_command() {
        let mut editor = Editor::default();
        let mut long = b"reboot".to_vec();
        long.resize(LINE - "yes".len(), b' ');
        long.extend_from_slice(b"yes please");
        let pressed = keys(&mut editor, &long);
        // Лишнее не принимается и не отображается.
        assert_eq!(pressed.len(), LINE);
        assert!(pressed.iter().all(|key| matches!(key, Key::Echo(_))));
        assert_eq!(keys(&mut editor, b"\r"), vec![Key::Enter(Command::Unknown)]);
        // Стёртый хвост потерянного не возвращает.
        keys(&mut editor, &long);
        assert_eq!(
            keys(&mut editor, b"\x7f\r"),
            vec![Key::Erase, Key::Enter(Command::Unknown)]
        );
        // Enter и Ctrl-C начинают новую строку.
        assert_eq!(
            keys(&mut editor, b"info\r").last(),
            Some(&Key::Enter(Command::Info))
        );
        keys(&mut editor, &long);
        assert_eq!(
            keys(&mut editor, b"\x03info\r").last(),
            Some(&Key::Enter(Command::Info))
        );
        // Строка, стёртая до пустой, — тоже новая: что на экране, то и
        // в ней, и Ctrl-D на ней завершает сессию.
        keys(&mut editor, &long);
        keys(&mut editor, &[0x7f; LINE]);
        assert_eq!(
            keys(&mut editor, b"info\r").last(),
            Some(&Key::Enter(Command::Info))
        );
        keys(&mut editor, &long);
        keys(&mut editor, &[0x08; LINE]);
        assert_eq!(keys(&mut editor, b"\x04"), vec![Key::Eof]);
        // Ровно `LINE` байт — ещё строка.
        let mut full = b"info".to_vec();
        full.resize(LINE, b' ');
        full.push(b'\r');
        assert_eq!(
            keys(&mut editor, &full).last(),
            Some(&Key::Enter(Command::Info))
        );
    }

    /// Консоль без ввода закрывается через [`SHELL_IDLE`], не раньше, и
    /// говорит клиенту почему; клиент, который не читает, прощание держит
    /// не дольше [`CLOSE_WAIT`].
    #[test]
    fn a_silent_shell_closes_after_idle() {
        const PROMPT: &str = "console, 'help' for commands\r\n> ";
        const FAREWELL: &str = "\r\nidle timeout, session closed\r\n";
        MockDriver::get().reset();
        let ((stack, _), _) = adapters_hw::fake::cable::pair();
        let info = device_info();
        let device = Device {
            info: &info,
            stack,
            // {%- if ota == "true" %}
            ota: crate::ota::tests::cmd().0,
            // {%- endif %}
        };
        let mut cx = Context::from_waker(Waker::noop());

        let mut reading = Silent {
            shown: Vec::new(),
            room: usize::MAX,
        };
        {
            let mut console = pin!(shell(&mut reading, &device));
            assert!(console.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(SHELL_IDLE - Duration::from_secs(1));
            assert!(console.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(Duration::from_secs(1));
            assert_eq!(console.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        }
        assert_eq!(reading.shown, format!("{PROMPT}{FAREWELL}").as_bytes());

        let mut deaf = Silent {
            shown: Vec::new(),
            room: PROMPT.len(),
        };
        let mut console = pin!(shell(&mut deaf, &device));
        assert!(console.as_mut().poll(&mut cx).is_pending());
        MockDriver::get().advance(SHELL_IDLE);
        assert!(console.as_mut().poll(&mut cx).is_pending());
        MockDriver::get().advance(CLOSE_WAIT);
        assert_eq!(console.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
    }

    /// Что терминал видит на ввод: эхо, стирание `\x08 \x08`, `^C`,
    /// исполненная команда с приглашением за ней; `exit`, Ctrl-D и EOF
    /// кончают сессию.
    #[test]
    fn the_shell_shows_what_the_editor_decides() {
        const PROMPT: &str = "console, 'help' for commands\r\n> ";
        let ((stack, _), _) = adapters_hw::fake::cable::pair();
        let info = device_info();
        let device = Device {
            info: &info,
            stack,
            // {%- if ota == "true" %}
            ota: crate::ota::tests::cmd().0,
            // {%- endif %}
        };
        let mut cx = Context::from_waker(Waker::noop());
        // Ввод, вывод и что осталось непрочитанным после конца сессии.
        let cases: [(&[u8], String, &[u8]); 4] = [
            (
                b"x\x7fbad\r\x03exit\rinfo\r",
                format!(
                    "{PROMPT}x\x08 \x08bad\r\nunknown command, 'help' for the list\r\n> \
                     ^C\r\n> exit\r\n"
                ),
                b"info\r",
            ),
            // Ctrl-D на набранной строке не обрывает сессию и не отображается.
            (b"ex\x04it\r", format!("{PROMPT}exit\r\n"), b""),
            (b"\x04info\r", PROMPT.to_owned(), b"info\r"),
            // EOF посреди строки — конец сессии без исполнения.
            (b"exi", format!("{PROMPT}exi"), b""),
        ];
        for (input, shown, left) in cases {
            let mut terminal = Script {
                input,
                shown: Vec::new(),
            };
            let console = pin!(shell(&mut terminal, &device)).poll(&mut cx);
            let input = input.escape_ascii();
            assert_eq!(console, Poll::Ready(Ok(())), "{input}");
            assert_eq!(
                terminal.shown.escape_ascii().to_string(),
                shown.as_bytes().escape_ascii().to_string(),
                "{input}"
            );
            assert_eq!(terminal.input, left, "{input}: ввод после конца сессии");
        }
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
        write_info(&mut out, &info, [0; 6], None, 0).unwrap();
        assert!(
            out.ends_with("\nlast panic dhcp.rs:12:5: lease lost\n"),
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

    /// Ключ не той длины — короче или длиннее 32 байт — заменяется новым и
    /// записывается; раздел, который не прочитался (здесь — значение длиннее
    /// буфера чтения), не перезаписывается: сбой мог быть временным.
    #[tokio::test]
    async fn only_a_missing_key_is_overwritten() {
        for len in [16, 33, 64] {
            let mut wrong = Stored(Memory(Some(vec![1; len])));
            let seed = wrong.seed().await;
            assert_eq!(wrong.0.0.as_deref(), Some(&seed[..]), "{len} байт");
        }

        let unreadable = vec![2; 200];
        let mut broken = Stored(Memory(Some(unreadable.clone())));
        broken.seed().await;
        assert_eq!(broken.0.0, Some(unreadable));
    }
    // {%- endif %}
}
