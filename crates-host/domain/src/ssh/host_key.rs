//! Ключ хоста ed25519: вшитый в прошивку или созданный платой и хранимый в
//! разделе настроек.

// {%- if ssh_host_key == "config" %}
use defmt_or_log::{info, warn};
use embedded_storage_async::nor_flash::NorFlash;
// {%- endif %}
use ports::HostKey;
// {%- if ssh_host_key == "config" %}

use crate::settings::Settings;

/// Ключ хоста в разделе настроек.
pub const HOST_KEY_ID: u32 = 0x0055_0001;

/// Буфер чтения ключа, байт. Больше ключа: значение под [`HOST_KEY_ID`]
/// другой длины (записанное чужой прошивкой) должно прочитаться и
/// замениться, а не отказать на чтении — отказ ключ не перезаписывает, и
/// каждый запуск получал бы временный. 128 — рабочий буфер
/// `crate::settings`: длиннее хранилище не запишет.
const READ_SCRATCH: usize = 128;
// {%- endif %}

/// Ключ хоста, вшитый в прошивку.
#[derive(Debug, Clone, Copy)]
pub struct Fixed(pub &'static [u8; 32]);

impl HostKey for Fixed {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт порт `HostKey`; вшитому ключу ждать нечего"
    )]
    async fn seed(&mut self) -> [u8; 32] {
        *self.0
    }
}
// {%- if ssh_host_key == "config" %}

/// Ключ хоста в хранилище настроек над носителем `F`: первый старт создаёт
/// его из `getrandom` и записывает под [`HOST_KEY_ID`], следующие — читают.
#[derive(Debug)]
pub struct Stored<F: NorFlash>(pub Settings<F>);

impl<F: NorFlash> HostKey for Stored<F> {
    async fn seed(&mut self) -> [u8; 32] {
        // Иначе ключ не записался бы ни разу и менялся бы с каждым сбросом.
        const {
            assert!(
                32 <= Settings::<F>::MAX_VALUE,
                "хранилище настроек не вмещает ключ хоста (32 байта)"
            );
        }
        let mut scratch = [0; READ_SCRATCH];
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
