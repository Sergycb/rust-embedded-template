//! Узел `SSH`: консоль платы по SSH.
//!
//! Одно соединение на порту [`PORT`](crate::ssh::PORT), пользователь
//! [`USER`](crate::ssh::USER), одна сессия: `shell` или `exec`. Вход — по
//! паролю, чей хэш вшит в прошивку
//! ([`PasswordHash`](crate::console::PasswordHash)). Команды:
//!
//! | Команда | Ответ |
//! |---|---|
//! | `help` | список команд |
//! | `info` | прошивка, чип, заводской номер, MAC, адрес, время работы, причина сброса, последняя паника |
// {%- if ota == "true" %}
//! | `reboot yes` | перезагрузка — командой узлу OTA; без `yes` — подсказка |
// {%- endif %}
//! | `exit` | закрывает сессию (как и EOF, Ctrl-D) |
//!
//! `shell` редактирует строку (стрелки, Home/End, Delete, Ctrl-U), помнит
//! историю (Up/Down) и дополняет команды по Tab; `help` и `help <команда>`
//! собираются из вариантов [`Command`](crate::ssh::Command).
//! Это отправная точка: свои команды проект добавляет вариантами
//! [`Command`](crate::ssh::Command) — разбор, `help`, историю и
//! автодополнение строит `bareminal`, — данные берёт у своих узлов клиентами
//! их входов{% if ota == "true" %}, как перезагрузку — у OTA{% endif %}.
//!
//! Протокол — `sunset`, его асинхронная обвязка — `sunset-async` над сокетом
//! TCP `embassy-net`. Ключ хоста
// {%- if ssh_host_key == "config" %}
//! ed25519 создаёт плата при первом старте и хранит в разделе настроек
//! ([`Stored`](crate::ssh::Stored)).
// {%- else %}
//! ed25519 вшит в прошивку (`ssh-host-key.bin`).
// {%- endif %}
//! Он не меняется от сброса к сбросу, и клиент один раз запоминает его в
//! `known_hosts`. Попыток входа `sunset` не считает — их ограничивает узел: шесть
//! неудач и 30 секунд на вход на соединение. Консоль без ввода закрывается через
//! 15 минут: соединение у узла одно.
//!
//! Чего в `sunset` 0.6 нет: `exit-status` (OpenSSH выходит с кодом 255) и
//! EOF/CLOSE канала по инициативе сервера — оба `sunset` шлёт только эхом на
//! пакеты клиента. Поэтому соединение после сессии закрывает узел, а вывод
//! `exec` встаёт в очередь раньше, чем разбирается EOF клиента (см.
//! `session`).

// {%- if ssh_host_key == "config" %}
mod host_key;
// {%- endif %}
mod session;

use defmt_or_log::{info, warn};
use embassy_net::tcp::TcpSocket;
use embassy_time::with_timeout;
// {%- if ssh_host_key == "config" %}
use embedded_storage_async::nor_flash::NorFlash;
// {%- endif %}
use sunset_async::SSHServer;
use sunset_async::sunset::SignKey;
use sunset_async::sunset::ed25519_dalek::SigningKey;

use crate::console::{Access, Device};
use crate::ssh::session::{CLOSE_WAIT, IDLE, KEEP_ALIVE, session};

pub use crate::console::{Command, Confirm, USER};
// {%- if ssh_host_key == "config" %}
pub use host_key::{HOST_KEY_ID, Stored};
// {%- endif %}
pub use session::{PORT, SSH_BUFFER, TCP_BUFFER};

// Узел объявлен рядом со своей задачей.
// {%- if ssh_host_key == "config" %}
// `<K>` — ключ хоста в хранилище над носителем платы (`Stored<…>`), которого
// `domain` не видит. `local` у ключа и у стека — оба вправе быть `!Send`.
// {%- else %}
// `local` у стека — он вправе быть `!Send`.
// {%- endif %}
// Буферы (~7 KiB) — слоты `= static`: через стек они не идут. Без
// `watchdog:`: сервер ждёт клиента, которого может не быть всю жизнь платы.
supervisor::supervisor_fragment! {
    // {%- if ssh_host_key == "config" %}
    name: SSH_FRAG<K>;
    // {%- else %}
    //{{newline}}    name: SSH_FRAG;
    // {%- endif %}
    boot: inputs: struct $crate::ssh::SshWiring;

    node resources: [
            /// Сетевой стек, на котором узел открывает свой сокет.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Хэш пароля пользователя.
            LOGIN: $crate::console::PasswordHash = inputs.login,
            /// Ключ хоста.
            // {%- if ssh_host_key == "config" %}
            HOST_KEY: local K = inputs.host_key,
            // {%- else %}
            //{{newline}}            HOST_KEY: &'static [u8; 32] = inputs.host_key,
            // {%- endif %}
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

/// Узел `SSH`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
// Заголовок по варианту: заголовок `file` — в той же строке, что `else`;
// иначе в проекте остался бы `//` перед собой с отступом блока, и
// `cargo fmt --check` упал бы.
// {%- if ssh_host_key == "config" %}
impl<F: NorFlash> SshCtx<'_, Stored<F>> {
    // {%- else %}{{newline}}impl SshCtx<'_> {
    // {%- endif %}
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
            stack: Some(stack),
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

// {%- if ssh_host_key == "config" %}
pub async fn task<F: NorFlash>(mut ctx: SshCtx<'_, Stored<F>>) {
    // {%- else %}{{newline}}pub async fn task(mut ctx: SshCtx<'_>) {
    // {%- endif %}
    info!("ssh: узел запущен");
    // Ключ хоста и доступ — подготовка на весь срок работы: от сессии к
    // сессии они не меняются.
    // {%- if ssh_host_key == "config" %}
    let seed = ctx.host_key.seed().await;
    // {%- else %}
    //{{newline}}    let seed = **ctx.host_key;
    // {%- endif %}
    let key = SignKey::Ed25519(SigningKey::from_bytes(&seed));
    let access = Access::new(*ctx.login);
    // Принимает соединение, ведёт его до конца, ждёт следующее — бесконечно.
    loop {
        ctx.connection(&key, access).await;
    }
}

#[cfg(test)]
mod tests {
    //! Тесты узла `SSH`: попытки входа, ключ хоста.

    use crate::ssh::session::tries_exhausted;
    // {%- if ssh_host_key == "config" %}
    use crate::settings::Settings;
    use crate::ssh::{HOST_KEY_ID, Stored};
    use adapters_hw::flash::fake::MemFlash;
    use embedded_storage_async::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
    // {%- endif %}
    // {%- if ssh_host_key == "config" %}

    /// Носитель настроек: две страницы по 256 байт, слово 8.
    type Flash = MemFlash<512, 8, 256>;

    /// Флеш, который не читается: запись и стирание — как у [`Flash`], чтение
    /// отказывает. Так разыгрывается раздел, который не прочитался.
    struct Unreadable(Flash);

    impl ErrorType for Unreadable {
        type Error = NorFlashErrorKind;
    }

    impl ReadNorFlash for Unreadable {
        const READ_SIZE: usize = 1;

        fn read(
            &mut self,
            _: u32,
            _: &mut [u8],
        ) -> impl Future<Output = Result<(), NorFlashErrorKind>> {
            core::future::ready(Err(NorFlashErrorKind::Other))
        }

        fn capacity(&self) -> usize {
            ReadNorFlash::capacity(&self.0)
        }
    }

    impl NorFlash for Unreadable {
        const WRITE_SIZE: usize = <Flash as NorFlash>::WRITE_SIZE;
        const ERASE_SIZE: usize = <Flash as NorFlash>::ERASE_SIZE;

        async fn erase(&mut self, from: u32, to: u32) -> Result<(), NorFlashErrorKind> {
            NorFlash::erase(&mut self.0, from, to).await
        }

        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), NorFlashErrorKind> {
            NorFlash::write(&mut self.0, offset, bytes).await
        }
    }
    // {%- endif %}

    // {%- if ssh_host_key == "config" %}
    /// Хранилище над флешем `flash`.
    fn settings<F: NorFlash>(flash: F) -> Settings<F> {
        Settings::new(flash, 0..512)
    }

    /// Ключ хоста, записанный в хранилище.
    async fn stored_key<F: NorFlash>(settings: &mut Settings<F>) -> Option<Vec<u8>> {
        let mut scratch = [0; 128];
        settings
            .read(HOST_KEY_ID, &mut scratch)
            .await
            .expect("чтение")
            .map(<[u8]>::to_vec)
    }
    // {%- endif %}

    /// Пять неудач — ещё попытка, шестая закрывает соединение.
    #[test]
    fn the_sixth_failure_closes_the_connection() {
        for failures in 0..6 {
            assert!(!tries_exhausted(failures), "{failures}");
        }
        assert!(tries_exhausted(6));
    }

    // {%- if ssh_host_key == "config" %}

    /// Первый старт создаёт ключ и записывает, следующий читает тот же.
    #[tokio::test]
    async fn the_stored_key_survives_a_restart() {
        let mut stored = Stored(settings(Flash::erased()));
        let first = stored.seed().await;
        assert_ne!(first, [0; 32]);
        assert_eq!(stored_key(&mut stored.0).await, Some(first.to_vec()));
        // Новое хранилище над тем же флешем — как после сброса.
        let mem = core::mem::take(&mut stored.0.flash().mem);
        assert_eq!(Stored(settings(Flash { mem })).seed().await, first);
    }

    /// Ключ не той длины — короче или длиннее 32 байт — заменяется новым и
    /// записывается; раздел, который не прочитался, не перезаписывается: сбой
    /// мог быть временным.
    #[tokio::test]
    async fn only_a_missing_key_is_overwritten() {
        for len in [16, 33, 64] {
            let mut wrong = Stored(settings(Flash::erased()));
            wrong
                .0
                .write(HOST_KEY_ID, &vec![1; len])
                .await
                .expect("запись");
            let seed = wrong.seed().await;
            assert_eq!(
                stored_key(&mut wrong.0).await,
                Some(seed.to_vec()),
                "{len} байт"
            );
        }

        let mut broken = Stored(settings(Unreadable(Flash::erased())));
        broken.seed().await;
        assert!(
            broken.0.flash().0.mem.iter().all(|b| *b == 0xFF),
            "непрочитанный раздел не тронут"
        );
    }
    // {%- endif %}
}
