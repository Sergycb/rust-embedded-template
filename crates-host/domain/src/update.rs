//! Применение подписанного обновления: порядок проверок, в котором каждый
//! отказ различим.
//!
//! Здесь, а не в адаптере: «принимать ли этот образ» — правило продукта, и
//! проверяется оно на хосте с фейком порта. Адаптер поверх `embassy-boot`
//! умеет только криптографию и пометку разделов
//! ([`SignedFirmwareUpdate::verify_and_mark_updated`](ports::SignedFirmwareUpdate::verify_and_mark_updated));
//! всё, что перед ней, — здесь. Без подписи применять нечего:
//! `board.ota.mark_updated()` через порт.

use ports::{SignedFirmwareUpdate, UpdateError};

use crate::firmware;

/// Сколько байт в хвосте образа занимает версия.
///
/// Хвост, а не фиксированное смещение внутри образа: своя секция сразу за
/// таблицей векторов накладывается на `.text` (линкер отказывается собирать),
/// а всё остальное в образе разъезжается от любой правки кода. Конец же
/// подписывается наравне с началом и адресуется одним вычитанием. Те же четыре
/// байта дописывает `cargo xtask build`, читая версию из символа
/// `FW_VERSION_IN_IMAGE` в ELF (`crates-host/xtask`).
pub const VERSION_BYTES: u32 = 4;

/// Проверяет принятый образ и, если всё сошлось, просит bootloader поменять
/// разделы местами на следующем сбросе. Сам сброс — за вызывающим.
///
/// `signature` и `len` приезжают вместе с образом по вашему каналу, а берутся
/// из сборки: `cargo xtask build` кладёт рядом с ELF сырой `app.bin`, его
/// подпись `app.bin.sig` и печатает длину. Подписан не образ, а его SHA-512.
///
/// Порядок проверок — не косметика, у каждого шага своя причина:
///
/// 1. **Длина — первой.** Присылает её тот же недоверенный канал, а
///    `verify_and_mark_updated` в embassy-boot начинается с
///    `assert!(update_len <= dfu.capacity())`, то есть с паники; в release
///    паника это сброс, и одно кривое число уводило бы устройство в цикл.
///    Сверяется с [`capacity`](ports::FirmwareUpdate::capacity), а это размер
///    `ACTIVE`, меньший `DFU`, — строже, чем `assert!`, и заодно отсекает
///    образ, который в приёмник влез бы, а при обмене оказался обрезан.
/// 2. **Занятость раздела — до версии.** В состояниях обмена и отката в `DFU`
///    лежит не присланный образ, а предыдущая прошивка; версия у неё заведомо
///    старее, и проверка версии честно ответила бы «откат» — соврав о причине.
/// 3. **Версия — до подписи.** Читается из ещё не проверенных байтов, но
///    подделать её нельзя: она внутри подписанного, и хеш считается по ней
///    тоже. После подписи проверять поздно — `verify_and_mark_updated` при
///    успехе сразу помечает разделы к обмену.
/// 4. **Версия — до ключа.** В шаблоне ключ по умолчанию нулевой; стой ключ
///    раньше, до версии дело не доходило бы, и target-тест
///    `ota_rejects_an_older_image` был бы вечно красным — защиту нечем было бы
///    проверить.
/// 5. **Нулевой ключ — до криптографии.** Это не «просто не совпадёт»: точка
///    малого порядка, подпись для которой подделывается перебором.
///
/// Не прошла проверка — разделы не меняются местами, устройство продолжает
/// работать на текущем образе. Именно поэтому подпись проверяется здесь, а не
/// в bootloader'е: тот прыгает безусловно, а отвергнуть чужой образ нужно ДО
/// того, как он стал активным.
pub fn apply_signed<F: SignedFirmwareUpdate>(
    flash: &mut F,
    signature: &[u8; 64],
    len: u32,
) -> Result<(), UpdateError<F::Error>> {
    let capacity = flash.capacity()?;
    if len > capacity {
        return Err(UpdateError::TooLong { len, capacity });
    }

    if flash.is_busy()? {
        return Err(UpdateError::Busy);
    }

    // Отдельная ветка, чтобы вычитание не ушло в отрицательное: в release оно
    // завернулось бы и увело чтение в конец раздела.
    let Some(offset) = len.checked_sub(VERSION_BYTES) else {
        return Err(UpdateError::Truncated { len });
    };
    // Выравнивать смещение не нужно: у `embassy-stm32` `READ_SIZE = 1` — одна
    // константа на все семейства, — так что чтение четырёх байт с любого
    // адреса законно. Появись у чипа требование пожёстче, здесь пришлось бы
    // дочитывать блок целиком.
    let mut raw = [0u8; VERSION_BYTES as usize];
    flash.read(offset, &mut raw)?;
    // Порядок байт — тот же, что пишет xtask: little-endian, как у Cortex-M.
    let incoming = u32::from_le_bytes(raw);
    let current = flash.running_version();
    if !firmware::is_upgrade(current, incoming) {
        return Err(UpdateError::Rollback { current, incoming });
    }

    if *flash.public_key() == [0; 32] {
        return Err(UpdateError::NoPublicKey);
    }

    flash.verify_and_mark_updated(signature, len)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{VERSION_BYTES, apply_signed};
    use crate::firmware::pack;
    use ports::{FirmwareUpdate, SignedFirmwareUpdate, UpdateError};

    const KEY: [u8; 32] = [7; 32];
    const SIGNATURE: [u8; 64] = [9; 64];

    /// Раздел с образом и настройками устройства; записывает, что у него
    /// спросили и о чём попросили.
    struct Fake {
        dfu: Vec<u8>,
        capacity: u32,
        busy: bool,
        version: u32,
        key: [u8; 32],
        verify: Result<(), &'static str>,
        verified: Vec<([u8; 64], u32)>,
    }

    impl Fake {
        fn with_image(len: u32, incoming: u32) -> Self {
            let mut dfu = vec![0xFF; 256];
            dfu[(len - VERSION_BYTES) as usize..len as usize]
                .copy_from_slice(&incoming.to_le_bytes());
            Self {
                dfu,
                capacity: 128,
                busy: false,
                version: pack(1, 2, 3),
                key: KEY,
                verify: Ok(()),
                verified: Vec::new(),
            }
        }
    }

    impl FirmwareUpdate for Fake {
        type Error = &'static str;

        fn write_granularity(&mut self) -> u32 {
            8
        }
        fn capacity(&mut self) -> Result<u32, Self::Error> {
            Ok(self.capacity)
        }
        fn is_busy(&mut self) -> Result<bool, Self::Error> {
            Ok(self.busy)
        }
        fn prepare(&mut self, _len: u32) -> Result<(), Self::Error> {
            Ok(())
        }
        fn write(&mut self, _offset: u32, _data: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            buf.copy_from_slice(&self.dfu[start..start + buf.len()]);
            Ok(())
        }
        fn mark_booted(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
        fn mark_updated(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl SignedFirmwareUpdate for Fake {
        fn running_version(&self) -> u32 {
            self.version
        }
        fn public_key(&self) -> &[u8; 32] {
            &self.key
        }
        fn verify_and_mark_updated(
            &mut self,
            signature: &[u8; 64],
            len: u32,
        ) -> Result<(), Self::Error> {
            self.verified.push((*signature, len));
            self.verify
        }
    }

    #[test]
    fn accepts_a_newer_signed_image() {
        let mut flash = Fake::with_image(64, pack(1, 2, 4));

        apply_signed(&mut flash, &SIGNATURE, 64).expect("образ новее и подписан");

        assert_eq!(flash.verified, vec![(SIGNATURE, 64)]);
    }

    /// Длина — первой, и своим вариантом: пока обе защиты отвечали одинаково,
    /// удаление одной из них тест не замечал.
    #[test]
    fn refuses_a_length_beyond_capacity_before_anything_else() {
        let mut flash = Fake::with_image(64, pack(1, 2, 4));
        flash.busy = true; // и занят, и ключ нулевой — а отказ всё равно по длине
        flash.key = [0; 32];

        let refused = apply_signed(&mut flash, &SIGNATURE, 129);

        assert_eq!(
            refused,
            Err(UpdateError::TooLong {
                len: 129,
                capacity: 128
            })
        );
        assert!(flash.verified.is_empty());
    }

    /// Занятый раздел — до версии: версия у образа отката заведомо старее, и
    /// ответ «откат» соврал бы о причине.
    #[test]
    fn refuses_a_busy_partition_instead_of_calling_it_a_rollback() {
        let mut flash = Fake::with_image(64, pack(0, 0, 1));
        flash.busy = true;

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::Busy)
        );
    }

    #[test]
    fn refuses_an_image_shorter_than_its_version_tail() {
        let mut flash = Fake::with_image(64, pack(1, 2, 4));

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 3),
            Err(UpdateError::Truncated { len: 3 })
        );
    }

    /// Та же версия — тоже откат: повторная заливка того же номера — типичный
    /// вид атаки.
    #[test]
    fn refuses_an_older_or_equal_image_before_the_key() {
        for incoming in [pack(1, 2, 3), pack(1, 1, 9)] {
            let mut flash = Fake::with_image(64, incoming);
            flash.key = [0; 32]; // ключ нулевой, а отказ — по версии

            assert_eq!(
                apply_signed(&mut flash, &SIGNATURE, 64),
                Err(UpdateError::Rollback {
                    current: pack(1, 2, 3),
                    incoming
                })
            );
        }
    }

    #[test]
    fn refuses_a_zero_key_before_cryptography() {
        let mut flash = Fake::with_image(64, pack(1, 2, 4));
        flash.key = [0; 32];

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::NoPublicKey)
        );
        assert!(flash.verified.is_empty(), "до криптографии дойти не должно");
    }

    #[test]
    fn passes_a_signature_failure_through() {
        let mut flash = Fake::with_image(64, pack(1, 2, 4));
        flash.verify = Err("подпись не сошлась");

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::Flash("подпись не сошлась"))
        );
    }
}
