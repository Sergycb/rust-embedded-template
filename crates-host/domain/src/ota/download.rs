//! Приём образа по кускам: всё, что одинаково для любого канала доставки.
//!
//! Канал у каждой платы свой — USB CDC, UART, сеть, SD-карта, — и шаблон его не
//! выбирает: канал — это порт [`ImageSource`](ports::ImageSource), который
//! реализует проект. Но вокруг канала есть работа, которая от него не зависит
//! вовсе, и делать её в каждом проекте заново незачем:
//!
//! * длину, пришедшую из недоверенного канала, надо сверить с тем, что
//!   устройство вообще способно принять, — до того, как стирать флеш;
//! * раздел надо подготовить ровно один раз, а не на каждый кусок;
//! * флеш принимает запись только словами (от четырёх до тридцати двух байт в
//!   зависимости от чипа), а канал отдаёт пакеты какой угодно длины — значит
//!   кто-то должен копить хвост до кратности;
//! * смещение надо вести самому и не дать образу вылезти за обещанную длину;
//! * последний кусок почти наверняка неполный, и его надо дописать, добив до
//!   слова.
//!
//! [`receive`](crate::ota::download::receive) делает всё перечисленное. Зовёт её
//! узел [`ota`](crate::ota) — из него же приходят длина (из
//! `ImageSource::begin`) и применение; напрямую, как ниже, — только проект
//! без графа:
//!
//! ```ignore
//! let len = domain::ota::download::receive(&mut ota.link, &mut ota.flash, announced_len).await?;
//! // дальше — применение обновления: `domain::ota::update::apply_signed` или
//! // `ota.flash.mark_updated()`, смотря выбрана ли подпись при генерации
//! ```
//!
//! Функция, а не структура с состоянием, по общему правилу проекта: домен —
//! логика над портами, а всё состояние приёма (сколько принято, хвост до
//! слова) живёт в локальных переменных одного вызова. Здесь, а не в `bsp`:
//! это правила приёма, а не свойство флеша, и проверяются они на хосте — с
//! фейками обоих портов, на которых можно разыграть и обрыв связи, и лишний
//! байт, и кусок в один байт.

use ports::{DownloadError, FirmwareUpdate, ImageSource, MAX_WRITE_SIZE};

/// Принимает образ длиной `announced` из `source` в `flash` и возвращает эту
/// же длину — её ждёт применение обновления.
///
/// Порядок в начале важен: длина сверяется с вместимостью ДО стирания, потому
/// что стирание уничтожает образ, в который устройство откатывается. Узнать,
/// что длина негодная, после этого — значит остаться без обеих прошивок.
///
/// Во флеш уходят только целые слова; остаток куска ждёт следующего. Последнее
/// неполное слово добивается единицами — фиксированным значением, и только
/// поэтому: называть `0xFF` «стёртым состоянием флеша» нельзя, на L0/L1 оно
/// нулевое (docs/flash.md), и там добор — самая обычная запись.
///
/// # Errors
///
/// Возвращает ошибку:
/// - `Empty` — если заявлена нулевая длина,
/// - `TooLong` — если длина больше вместимости раздела или добитая длина не влезает,
/// - `UnusableGranularity` — если порт вернул негодную гранулярность,
/// - `TooMuchData` — если прислали больше, чем обещали,
/// - `Incomplete` — если передача прервалась раньше обещанной длины,
/// - `Source` — если канал вернул ошибку,
/// - `Flash` — если флеш вернул ошибку.
///
/// # Panics
///
/// Паникует, если внутренние счётчики превышают `u32::MAX`. На 32-битных
/// платформах `usize` может быть шире `u32`, а размеры образов ограничены
/// аппаратными пределами (максимальный сектор флеша — 256 Кбайт), поэтому
/// на практике это невозможно.
pub async fn receive<S: ImageSource, F: FirmwareUpdate>(
    source: &mut S,
    flash: &mut F,
    announced: u32,
) -> Result<u32, DownloadError<S::Error, F::Error>> {
    if announced == 0 {
        return Err(DownloadError::Empty);
    }

    let capacity = flash.capacity().map_err(DownloadError::Flash)?;
    if announced > capacity {
        return Err(DownloadError::TooLong {
            announced,
            capacity,
        });
    }

    // Ноль отвергается вместе со слишком широким словом, и это не симметрия
    // ради симметрии: на нуле первый же кусок делил бы на ноль, а деление
    // паникует и в release — то есть уводит устройство в сброс.
    let word = flash.write_granularity();
    if word == 0 || word as usize > MAX_WRITE_SIZE {
        return Err(DownloadError::UnusableGranularity(word));
    }
    let word_len = word as usize;

    // Добитая до слова длина тоже обязана помещаться: хвост пишется целым
    // словом, и на образе, чья длина не кратна слову, запись ушла бы за
    // границу. У настоящего раздела это не случается (он кратен слову), но
    // порт такого не обещает.
    if announced.div_ceil(word).saturating_mul(word) > capacity {
        return Err(DownloadError::TooLong {
            announced,
            capacity,
        });
    }

    flash
        .prepare(announced)
        .await
        .map_err(DownloadError::Flash)?;

    let mut written: u32 = 0;
    // Хвост, не добравший до целого слова (буфер на самое широкое слово
    // чипа — гранулярность известна только в рантайме). Первые `pending`
    // байт значимы.
    let mut tail = [0u8; MAX_WRITE_SIZE];
    let mut pending: usize = 0;

    while let Some(mut chunk) = source.next().await.map_err(DownloadError::Source)? {
        let received = (written
            + u32::try_from(pending).expect("pending не должен превышать u32::MAX"))
        .saturating_add(
            u32::try_from(chunk.len()).expect("chunk.len() не должен превышать u32::MAX"),
        );
        if received > announced {
            return Err(DownloadError::TooMuchData {
                announced,
                received,
            });
        }

        // Сначала добить хвост прошлого куска: пока он неполон, писать во флеш
        // нечего, а порядок байт в образе обязан сохраниться.
        if pending > 0 {
            let need = word_len - pending;
            let take = need.min(chunk.len());
            tail[pending..pending + take].copy_from_slice(&chunk[..take]);
            pending += take;
            chunk = &chunk[take..];

            if pending < word_len {
                continue;
            }
            flash
                .write(written, &tail[..word_len])
                .await
                .map_err(DownloadError::Flash)?;
            written += word;
            // `pending` не обнуляется здесь явно: до его следующего чтения
            // ниже безусловно пишет `pending = rest.len()` — явный ноль был
            // бы мёртвой записью, на которую `-D warnings` отвечает
            // `unused_assignments` (в отличие от поля структуры в прежней
            // версии, эту запись в локальной переменной видит rustc).
        }

        // Целые слова — прямо во флеш, без копирования через буфер.
        let whole = chunk.len() - chunk.len() % word_len;
        if whole > 0 {
            flash
                .write(written, &chunk[..whole])
                .await
                .map_err(DownloadError::Flash)?;
            written += u32::try_from(whole).expect("whole не должен превышать u32::MAX");
        }

        // Остаток короче слова ждёт следующего куска.
        let rest = &chunk[whole..];
        tail[..rest.len()].copy_from_slice(rest);
        pending = rest.len();
    }

    let received = written + u32::try_from(pending).expect("pending не должен превышать u32::MAX");
    if received != announced {
        return Err(DownloadError::Incomplete {
            announced,
            received,
        });
    }

    if pending > 0 {
        tail[pending..word_len].fill(0xFF);
        flash
            .write(written, &tail[..word_len])
            .await
            .map_err(DownloadError::Flash)?;
    }
    Ok(announced)
}

#[cfg(test)]
#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
mod tests {
    use ports::MAX_WRITE_SIZE;

    use super::receive;
    use crate::ota::fakes::{FakeFlash, FakeLink};
    use ports::{DownloadError, FirmwareUpdate};

    fn image(len: u32, seed: u32) -> Vec<u8> {
        (0..len).map(|i| (i * seed) as u8).collect()
    }

    proptest::proptest! {
        /// Как бы канал ни нарезал образ и какой бы ширины ни было слово флеша,
        /// во флеше оказывается ровно образ, добитый `0xFF` до целого слова.
        /// Три теста выше проверяют по одной нарезке, здесь их тысячи.
        #[test]
        fn any_chunking_reassembles_the_image(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 1..300),
            sizes in proptest::collection::vec(1usize..=40, 1..20),
            word in proptest::sample::select(vec![1u32, 2, 4, 8, 16, 32]),
        ) {
            let mut rest = data.as_slice();
            let mut chunks = Vec::new();
            for size in sizes.iter().cycle() {
                if rest.is_empty() {
                    break;
                }
                let (head, tail) = rest.split_at((*size).min(rest.len()));
                chunks.push(head.to_vec());
                rest = tail;
            }
            let mut source = FakeLink::of(chunks);
            let mut flash = FakeFlash::new(1024, word);

            let len = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("рантайм")
                .block_on(receive(&mut source, &mut flash, data.len() as u32))
                .expect("приём");

            proptest::prop_assert_eq!(len, data.len() as u32);
            proptest::prop_assert_eq!(&flash.memory[..data.len()], &data[..]);
            let padded = data.len().next_multiple_of(word as usize);
            proptest::prop_assert!(flash.memory[data.len()..padded].iter().all(|b| *b == 0xFF));
        }
    }

    /// Принять образ кусками по одному байту — самый недружелюбный случай,
    /// какой может выдать канал.
    #[tokio::test]
    async fn reassembles_an_image_from_single_byte_chunks() {
        let image = image(100, 1);
        let mut source = FakeLink::of(image.iter().map(|b| vec![*b]));
        let mut flash = FakeFlash::new(1024, 8);

        let len = receive(&mut source, &mut flash, image.len() as u32)
            .await
            .expect("приём должен завершиться");

        assert_eq!(len, image.len() as u32);
        assert_eq!(&flash.memory[..image.len()], &image[..]);
    }

    /// Куски, не кратные ни слову, ни друг другу.
    #[tokio::test]
    async fn reassembles_an_image_from_ragged_chunks() {
        let image = image(200, 7);
        let mut source = FakeLink::of(image.chunks(13).map(<[u8]>::to_vec));
        let mut flash = FakeFlash::new(1024, 8);

        receive(&mut source, &mut flash, image.len() as u32)
            .await
            .expect("приём");

        assert_eq!(&flash.memory[..image.len()], &image[..]);
        // Всё, что дошло до флеша, было кратно слову — иначе фейк отказал бы;
        // здесь же проверяется, что запись вообще шла словами, а не одним
        // куском в конце.
        assert!(flash.writes.len() > 1, "запись должна идти по мере приёма");
    }

    /// Слово в 32 байта (самое широкое у STM32) и образ, не кратный ему.
    #[tokio::test]
    async fn pads_the_last_word_of_an_unaligned_image() {
        let image = image(70, 1);
        let mut source = FakeLink::of([image.clone()]);
        let mut flash = FakeFlash::new(1024, MAX_WRITE_SIZE as u32);

        receive(&mut source, &mut flash, image.len() as u32)
            .await
            .expect("приём");

        assert_eq!(&flash.memory[..image.len()], &image[..]);
        // Хвост добит единицами — фиксированным значением, а не мусором.
        assert_eq!(&flash.memory[image.len()..96], &[0xFF; 26][..]);
    }

    /// Нулевая длина отвергается здесь, а не оставляется реализации порта:
    /// `prepare(0)` не стёр бы ничего, и первая же запись легла бы в нестёртую
    /// память, на F2/F4/F7 молча.
    #[tokio::test]
    async fn refuses_an_empty_image_before_touching_the_partition() {
        let mut source = FakeLink::of([]);
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = receive(&mut source, &mut flash, 0).await;

        assert_eq!(refused, Err(DownloadError::Empty));
        assert_eq!(flash.prepared, None, "раздел не должен быть подготовлен");
        assert!(
            flash.memory.iter().all(|b| *b == 0xA5),
            "раздел не должен быть стёрт"
        );
    }

    /// Образ длиннее раздела отвергается ДО стирания: иначе устройство
    /// осталось бы и без нового образа, и без того, куда откатываться.
    #[tokio::test]
    async fn refuses_an_image_longer_than_the_partition_without_erasing() {
        let mut source = FakeLink::of([]);
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = receive(&mut source, &mut flash, 65).await;

        assert_eq!(
            refused,
            Err(DownloadError::TooLong {
                announced: 65,
                capacity: 64
            })
        );
        assert_eq!(flash.prepared, None);
        assert!(flash.memory.iter().all(|b| *b == 0xA5));
    }

    /// Канал прислал больше, чем обещал: приём прекращается на том куске, где
    /// это стало видно, — канал дальше не читается.
    #[tokio::test]
    async fn refuses_more_data_than_announced() {
        let mut source = FakeLink::of([vec![0; 16], vec![0; 1], vec![0; 100]]);
        let mut flash = FakeFlash::new(1024, 8);

        let refused = receive(&mut source, &mut flash, 16).await;

        assert_eq!(
            refused,
            Err(DownloadError::TooMuchData {
                announced: 16,
                received: 17
            })
        );
        assert_eq!(
            source.next, 2,
            "после лишнего куска канал больше не читается"
        );
    }

    /// Передача оборвалась (канал ответил «конец» раньше времени): делать вид,
    /// что всё в порядке, нельзя.
    #[tokio::test]
    async fn refuses_an_incomplete_transfer() {
        let mut source = FakeLink::of([vec![0; 20]]);
        let mut flash = FakeFlash::new(1024, 8);

        let refused = receive(&mut source, &mut flash, 32).await;

        assert_eq!(
            refused,
            Err(DownloadError::Incomplete {
                announced: 32,
                received: 20
            })
        );
    }

    /// Гранулярность, с которой работать нельзя, отвергается до стирания.
    #[tokio::test]
    async fn refuses_an_unusable_write_granularity() {
        for word in [0, u32::try_from(MAX_WRITE_SIZE).unwrap() + 1] {
            let mut source = FakeLink::of([]);
            let mut flash = FakeFlash::new(1024, word.max(1));
            flash.word = word;

            let refused = receive(&mut source, &mut flash, 32).await;

            assert_eq!(refused, Err(DownloadError::UnusableGranularity(word)));
            assert_eq!(flash.prepared, None);
        }
    }

    /// Образ, чья добитая до слова длина не влезает, отвергается тоже: иначе
    /// последнее слово ушло бы за границу раздела.
    #[tokio::test]
    async fn refuses_an_image_whose_padded_length_overflows() {
        let mut source = FakeLink::of([]);
        let mut flash = FakeFlash::new(100, 32);

        let refused = receive(&mut source, &mut flash, 100).await;

        assert_eq!(
            refused,
            Err(DownloadError::TooLong {
                announced: 100,
                capacity: 100
            })
        );
    }

    /// Отказ флеша доходит до вызывающего как есть, а не теряется.
    #[tokio::test]
    async fn passes_a_flash_failure_through() {
        let mut source = FakeLink::of([vec![0; 8]]);
        // Обёртка, у которой `prepare` «проходит», но флаг подготовки не
        // ставит: первая же запись фейка ответит отказом, и он обязан дойти
        // до вызывающего как `Flash(_)`.
        #[expect(
            clippy::items_after_statements,
            reason = "локальный тип теста стоит рядом с использованием"
        )]
        struct NoPrepare(FakeFlash);
        #[expect(
            clippy::items_after_statements,
            reason = "локальный тип теста стоит рядом с использованием"
        )]
        impl FirmwareUpdate for NoPrepare {
            type Error = &'static str;
            fn write_granularity(&mut self) -> u32 {
                self.0.write_granularity()
            }
            fn capacity(&mut self) -> Result<u32, Self::Error> {
                self.0.capacity()
            }
            async fn is_busy(&mut self) -> Result<bool, Self::Error> {
                self.0.is_busy().await
            }
            fn prepare(&mut self, _len: u32) -> impl Future<Output = Result<(), Self::Error>> {
                // «подготовил», но флаг не поставил — запись откажет
                std::future::ready(Ok(()))
            }
            async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
                self.0.write(offset, data).await
            }
            async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
                self.0.read(offset, buf).await
            }
            async fn mark_booted(&mut self) -> Result<(), Self::Error> {
                self.0.mark_booted().await
            }
            async fn mark_updated(&mut self) -> Result<(), Self::Error> {
                self.0.mark_updated().await
            }
        }
        let mut flash = NoPrepare(FakeFlash::new(1024, 8));

        let failed = receive(&mut source, &mut flash, 8).await;

        assert_eq!(
            failed,
            Err(DownloadError::Flash("запись без подготовки раздела"))
        );
    }

    /// Отказ канала — тоже, и своим вариантом: кто именно отказал, важнее
    /// удобства одного `From`.
    #[tokio::test]
    async fn passes_a_source_failure_through() {
        let mut source = FakeLink::of([vec![0; 8], vec![0; 8]]);
        source.fail_at = Some(1);
        let mut flash = FakeFlash::new(1024, 8);

        let failed = receive(&mut source, &mut flash, 16).await;

        assert_eq!(failed, Err(DownloadError::Source("обрыв канала")));
    }
}
