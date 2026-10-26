//! Ключ хоста ed25519: вшитый в прошивку или созданный платой и хранимый в
//! разделе настроек.

// {%- if ssh_host_key == "config" %}
use defmt_or_log::{info, warn};

/// Ключ хоста в разделе настроек.
pub const HOST_KEY_ID: u32 = 0x0055_0001;
// {%- endif %}

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
        // увидели бы «REMOTE HOST IDENTIFICATION HAS CHANGED».
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
