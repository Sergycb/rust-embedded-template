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

/// Узел `SSH`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
pub async fn task<K: HostKey>(mut ctx: SshCtx<'_, K>) {
    ctx.run().await;
}

#[cfg(test)]
mod tests {
    //! Тесты узла `SSH`: ключ пользователя, команды, `info`, ключ хоста.

    use crate::device::{DeviceInfo, PANIC_BRIEF};
    use crate::ssh::console::{INFO_TEXT, parse, write_info};
    use crate::ssh::{Command, ed25519_public_key};
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
