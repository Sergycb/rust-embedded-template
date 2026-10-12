//! Применение подписанного обновления: порядок проверок, в котором каждый
//! отказ различим.
//!
//! Здесь, а не в адаптере: «принимать ли этот образ» — правило продукта, и
//! проверяется оно на хосте с фейком порта. Адаптер поверх `embassy-boot`
//! умеет только криптографию и пометку разделов
//! ([`SignedFirmwareUpdate::verify_and_mark_updated`](ports::SignedFirmwareUpdate::verify_and_mark_updated));
//! всё, что перед ней, — здесь. Без подписи применять нечего:
//! `FirmwareUpdate::mark_updated` адаптера платы.

use ports::{SignedFirmwareUpdate, UpdateError, VerifyError};

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
///
/// # Errors
///
/// Возвращает ошибку:
/// - `TooLong` — если длина больше вместимости раздела,
/// - `Busy` — если раздел обновления занят образом отката,
/// - `Truncated` — если длина меньше хвоста с версией,
/// - `Rollback` — если версия не новее текущей,
/// - `NoPublicKey` — если открытый ключ не подставлен (нули),
/// - `BadSignature` — если подпись не сошлась,
/// - `Flash` — если адаптер флеша вернул ошибку.
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

    flash
        .verify_and_mark_updated(signature, len)
        .map_err(|err| match err {
            VerifyError::Flash(err) => UpdateError::Flash(err),
            // `BadSignature` и незнакомые варианты (перечисление
            // `non_exhaustive`): отказ проверки — это отказ, а не пропуск образа.
            _ => UpdateError::BadSignature,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply_signed;
    use crate::firmware::pack;
    use crate::test_support::{FakeFlash, SIGNATURE};
    use ports::{UpdateError, VerifyError};
    use rstest::rstest;

    use crate::update::VERSION_BYTES;

    #[test]
    fn accepts_a_newer_signed_image() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));

        apply_signed(&mut flash, &SIGNATURE, 64).expect("образ новее и подписан");

        assert_eq!(flash.verified, vec![(SIGNATURE, 64)]);
    }

    /// Длина — первой, и своим вариантом: пока обе защиты отвечали одинаково,
    /// удаление одной из них тест не замечал.
    #[test]
    fn refuses_a_length_beyond_capacity_before_anything_else() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
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
        let mut flash = FakeFlash::with_image(64, pack(0, 0, 1));
        flash.busy = true;

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::Busy)
        );
    }

    #[test]
    fn refuses_an_image_shorter_than_its_version_tail() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));

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
            let mut flash = FakeFlash::with_image(64, incoming);
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
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.key = [0; 32];

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::NoPublicKey)
        );
        assert!(flash.verified.is_empty(), "до криптографии дойти не должно");
    }

    /// Подпись не сошлась — свой вариант, а не `Flash(_)`: отправителю
    /// важно отличить «исправь ключ» от «устройство неисправно».
    #[test]
    fn reports_a_bad_signature_as_such() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.verify = Err(VerifyError::BadSignature);

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::BadSignature)
        );
    }

    /// Отказ флеша внутри проверки доходит как отказ флеша.
    #[test]
    fn passes_a_flash_failure_inside_verify_through() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.verify = Err(VerifyError::Flash("флеш отказал"));

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::Flash("флеш отказал"))
        );
    }

    type FlashErr = &'static str;

    /// Матрица проверок `apply_signed`:
    /// каждая строка — независимый сценарий с набором параметров.
    #[rstest]
    #[case::too_long(
        /* len */ 129,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 2, 4),
        /* key */ [1u8; 32],
        /* verify */ Ok(()),
        /* expected */ UpdateError::<FlashErr>::TooLong { len: 129, capacity: 128 }
    )]
    #[case::busy(
        /* len */ 64,
        /* capacity */ 128,
        /* busy */ true,
        /* incoming_version */ pack(1, 2, 4),
        /* key */ [1u8; 32],
        /* verify */ Ok(()),
        /* expected */ UpdateError::<FlashErr>::Busy
    )]
    #[case::truncated(
        /* len */ 3,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 2, 4),
        /* key */ [1u8; 32],
        /* verify */ Ok(()),
        /* expected */ UpdateError::<FlashErr>::Truncated { len: 3 }
    )]
    #[case::rollback_older(
        /* len */ 64,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 1, 9),
        /* key */ [1u8; 32],
        /* verify */ Ok(()),
        /* expected */ UpdateError::<FlashErr>::Rollback { current: pack(1, 2, 3), incoming: pack(1, 1, 9) }
    )]
    #[case::rollback_equal(
        /* len */ 64,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 2, 3),
        /* key */ [1u8; 32],
        /* verify */ Ok(()),
        /* expected */ UpdateError::<FlashErr>::Rollback { current: pack(1, 2, 3), incoming: pack(1, 2, 3) }
    )]
    #[case::no_public_key(
        /* len */ 64,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 2, 4),
        /* key */ [0u8; 32],
        /* verify */ Ok(()),
        /* expected */ UpdateError::<FlashErr>::NoPublicKey
    )]
    #[case::bad_signature(
        /* len */ 64,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 2, 4),
        /* key */ [1u8; 32],
        /* verify */ Err(VerifyError::<FlashErr>::BadSignature),
        /* expected */ UpdateError::<FlashErr>::BadSignature
    )]
    #[case::flash_error(
        /* len */ 64,
        /* capacity */ 128,
        /* busy */ false,
        /* incoming_version */ pack(1, 2, 4),
        /* key */ [1u8; 32],
        /* verify */ Err(VerifyError::<FlashErr>::Flash("флеш отказал")),
        /* expected */ UpdateError::<FlashErr>::Flash("флеш отказал")
    )]
    fn apply_signed_rejects_when(
        #[case] len: u32,
        #[case] capacity: u32,
        #[case] busy: bool,
        #[case] incoming_version: u32,
        #[case] key: [u8; 32],
        #[case] verify: Result<(), VerifyError<FlashErr>>,
        #[case] expected: UpdateError<FlashErr>,
    ) {
        // `with_image` пишет версию в хвост образа длиной `len`, поэтому
        // передаём туда тот же `len`, что пойдёт в `apply_signed`.
        // Для `len < VERSION_BYTES` версию писать некуда — создаём флеш вручную.
        let mut flash = if len >= VERSION_BYTES {
            FakeFlash::with_image(len, incoming_version)
        } else {
            FakeFlash::new(capacity as usize, 8)
        };
        flash.busy = busy;
        flash.key = key;
        flash.verify = verify;

        let result = apply_signed(&mut flash, &SIGNATURE, len);

        assert_eq!(result, Err(expected));
        if !matches!(
            &result,
            Err(UpdateError::BadSignature | UpdateError::Flash(_))
        ) {
            assert!(
                flash.verified.is_empty(),
                "verify не должен вызываться для {result:?}"
            );
        }
    }

    #[rstest]
    #[case::accept1(64, false, pack(1, 2, 4), [1u8; 32])]
    #[case::accept2(128, false, pack(2, 0, 0), [2u8; 32])]
    fn apply_signed_accepts(
        #[case] len: u32,
        #[case] busy: bool,
        #[case] incoming_version: u32,
        #[case] key: [u8; 32],
    ) {
        let mut flash = FakeFlash::with_image(len, incoming_version);
        flash.busy = busy;
        flash.key = key;

        apply_signed(&mut flash, &SIGNATURE, len).expect("должно принять");

        assert_eq!(flash.verified, vec![(SIGNATURE, len)]);
    }
}
