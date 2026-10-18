//! Применение подписанного обновления: порядок проверок, в котором каждый
//! отказ различим.
//!
//! Здесь, а не в адаптере: «принимать ли этот образ» — правило продукта, и
//! проверяется оно на хосте с фейком порта. Подпись приложение не проверяет —
//! это делает bootloader перед обменом (`domain::ota::boot::screen_update`), а
//! отсюда ему уходит запись с подписью и длиной ([`PendingImage`](ports::PendingImage)) — обычной
//! записью порта в запасную страницу раздела. Без подписи применять нечего:
//! `FirmwareUpdate::mark_updated` адаптера платы.

use ports::{FirmwareUpdate, PendingImage, UpdateError};

use crate::ota::version;

/// Сколько байт в хвосте образа занимает версия.
///
/// Хвост, а не фиксированное смещение внутри образа: своя секция сразу за
/// таблицей векторов накладывается на `.text` (линкер отказывается собирать),
/// а всё остальное в образе разъезжается от любой правки кода. Конец же
/// подписывается наравне с началом и адресуется одним вычитанием. Те же четыре
/// байта дописывает `cargo xtask build`, читая версию из символа
/// `FW_VERSION_IN_IMAGE` в ELF (`crates-host/xtask`).
pub const VERSION_BYTES: u32 = 4;

/// Проверяет принятый образ и, если всё сошлось, оставляет bootloader'у
/// подпись и просит поменять разделы местами на следующем сбросе. Сам сброс —
/// за вызывающим.
///
/// `signature` и `len` приезжают вместе с образом по вашему каналу, а берутся
/// из сборки: `cargo xtask build` кладёт рядом с ELF сырой `app.bin`, его
/// подпись `app.bin.sig` и печатает длину. Подписан не образ, а его SHA-512.
/// `running_version` — версия этой прошивки (`FW_VERSION` приложения).
///
/// Запись о подписи ложится в запасную страницу раздела, по смещению
/// [`capacity`](ports::FirmwareUpdate::capacity) — её стирает `prepare`
/// адаптера проекта с подписью (`adapters::ota::Updater<_, _, true>`), — и только
/// потом помечается обмен: bootloader, увидев запрос, обязан найти и запись.
///
/// Порядок проверок — не косметика, у каждого шага своя причина:
///
/// 1. **Длина — первой.** Присылает её тот же недоверенный канал. Сверяется с
///    [`capacity`](ports::FirmwareUpdate::capacity), а это размер `ACTIVE`,
///    меньший `DFU`, — так отсекается образ, который в приёмник влез бы, а при
///    обмене оказался обрезан.
/// 2. **Занятость раздела — до версии.** В состояниях обмена и отката в `DFU`
///    лежит не присланный образ, а предыдущая прошивка; версия у неё заведомо
///    старее, и проверка версии честно ответила бы «откат» — соврав о причине.
/// 3. **Версия — здесь, а не в bootloader'е.** Bootloader не знает версию
///    текущего образа (записи о нём у него нет — только о новом), а приложение
///    знает свою. Читается она из ещё не проверенных байтов, но подделать её
///    нельзя: она внутри подписанного, и bootloader, проверяя подпись, проверит
///    и её.
///
/// Подпись не проверяется вовсе: это делает bootloader перед обменом, а
/// отвергнутый им образ просто не станет активным. Отправитель узнает об этом
/// по версии после перезагрузки, а не кодом отказа.
///
/// # Errors
///
/// Возвращает ошибку:
/// - `TooLong` — если длина больше вместимости раздела,
/// - `Busy` — если раздел обновления занят образом отката,
/// - `Truncated` — если длина меньше хвоста с версией,
/// - `Rollback` — если версия не новее текущей,
/// - `Flash` — если адаптер флеша вернул ошибку.
pub async fn apply_signed<F: FirmwareUpdate>(
    flash: &mut F,
    running_version: u32,
    signature: &[u8; 64],
    len: u32,
) -> Result<(), UpdateError<F::Error>> {
    let capacity = flash.capacity()?;
    if len > capacity {
        return Err(UpdateError::TooLong { len, capacity });
    }

    if flash.is_busy().await? {
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
    flash.read(offset, &mut raw).await?;
    // Порядок байт — тот же, что пишет xtask: little-endian, как у Cortex-M.
    let incoming = u32::from_le_bytes(raw);
    let current = running_version;
    if !version::is_upgrade(current, incoming) {
        return Err(UpdateError::Rollback { current, incoming });
    }

    let record = PendingImage {
        len,
        signature: *signature,
    };
    flash.write(capacity, &record.to_bytes()).await?;
    flash.mark_updated().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply_signed;
    use crate::ota::fakes::{FakeFlash, SIGNATURE};
    use crate::ota::version::pack;
    use ports::UpdateError;

    /// Версия «этой прошивки» в тестах.
    const CURRENT: u32 = pack(1, 2, 3);

    #[tokio::test]
    async fn records_a_newer_signed_image_for_the_bootloader() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));

        apply_signed(&mut flash, CURRENT, &SIGNATURE, 64)
            .await
            .expect("образ новее");

        assert_eq!(flash.recorded, vec![(SIGNATURE, 64)]);
        assert_eq!(flash.updated, 1, "обмен помечен после записи");
    }

    /// Длина — первой, и своим вариантом: пока защиты отвечали одинаково,
    /// удаление одной из них тест не замечал.
    #[tokio::test]
    async fn refuses_a_length_beyond_capacity_before_anything_else() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.busy = true; // и занят — а отказ всё равно по длине

        let refused = apply_signed(&mut flash, CURRENT, &SIGNATURE, 129).await;

        assert_eq!(
            refused,
            Err(UpdateError::TooLong {
                len: 129,
                capacity: 128
            })
        );
        assert!(flash.recorded.is_empty());
    }

    /// Занятый раздел — до версии: версия у образа отката заведомо старее, и
    /// ответ «откат» соврал бы о причине.
    #[tokio::test]
    async fn refuses_a_busy_partition_instead_of_calling_it_a_rollback() {
        let mut flash = FakeFlash::with_image(64, pack(0, 0, 1));
        flash.busy = true;

        assert_eq!(
            apply_signed(&mut flash, CURRENT, &SIGNATURE, 64).await,
            Err(UpdateError::Busy)
        );
    }

    #[tokio::test]
    async fn refuses_an_image_shorter_than_its_version_tail() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));

        assert_eq!(
            apply_signed(&mut flash, CURRENT, &SIGNATURE, 3).await,
            Err(UpdateError::Truncated { len: 3 })
        );
    }

    /// Та же версия — тоже откат: повторная заливка того же номера — типичный
    /// вид атаки.
    #[tokio::test]
    async fn refuses_an_older_or_equal_image() {
        for incoming in [pack(1, 2, 3), pack(1, 1, 9)] {
            let mut flash = FakeFlash::with_image(64, incoming);

            assert_eq!(
                apply_signed(&mut flash, CURRENT, &SIGNATURE, 64).await,
                Err(UpdateError::Rollback {
                    current: CURRENT,
                    incoming
                })
            );
            assert!(flash.recorded.is_empty());
            assert_eq!(flash.updated, 0);
        }
    }

    /// Отказ флеша при записи для bootloader'а доходит как отказ флеша, и
    /// обмен без записи не помечается.
    #[tokio::test]
    async fn passes_a_flash_failure_while_recording_through() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.record = Err("флеш отказал");

        assert_eq!(
            apply_signed(&mut flash, CURRENT, &SIGNATURE, 64).await,
            Err(UpdateError::Flash("флеш отказал"))
        );
        assert_eq!(flash.updated, 0, "обмен без записи не помечается");
    }
}
