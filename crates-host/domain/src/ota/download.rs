//! Приём образа по кускам: всё, что одинаково для любого протокола.
//!
//! Образ приходит командами ([`OtaCommand`](crate::ota::OtaCommand)): узел протокола переводит
//! свои сообщения в `Start`, `Chunk` и `Finish`, а узел [`ota`](crate::ota)
//! ведёт приём шагами [`Transfer`]. Вокруг этих шагов есть работа, которая от
//! протокола не зависит вовсе, и делать её в каждом проекте заново незачем:
//!
//! * длину, пришедшую из недоверенного канала, надо сверить с тем, что
//!   устройство вообще способно принять, — до того, как стирать флеш;
//! * раздел надо подготовить ровно один раз, а не на каждый кусок;
//! * флеш принимает запись только словами (от четырёх до тридцати двух байт в
//!   зависимости от чипа), а куски приходят какой угодно длины — значит
//!   кто-то должен копить хвост до кратности;
//! * смещение надо вести самому и не дать образу вылезти за обещанную длину;
//! * последний кусок почти наверняка неполный, и его надо дописать, добив до
//!   слова.
//!
//! [`Transfer`] делает всё перечисленное. Это не самостоятельный объект, а
//! данные состояния `Receiving` автомата узла OTA: между двумя командами приём
//! живёт там, а шаги — функции над адаптером [`Updater`]. Здесь, а не в
//! `bsp`: это правила приёма, а не свойство флеша, и проверяются они на
//! хосте — над разделами в памяти, на которых можно разыграть и лишний байт, и
//! кусок в один байт.

use embedded_storage_async::nor_flash::NorFlash;
use ports::{MAX_WRITE_SIZE, Partitions};

use crate::ota::updater::{FlashError, Updater};

/// Приём одного образа между `Start` и `Finish` — данные состояния
/// `Receiving` узла OTA.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// Обещанная длина образа.
    announced: u32,
    /// Слово записи флеша — проверено в [`start`](Self::start).
    word: u32,
    /// Сколько байт уже во флеше — всегда кратно слову.
    written: u32,
    /// Хвост, не добравший до целого слова (буфер на самое широкое слово
    /// чипа: гранулярность известна только в рантайме). Первые `pending` байт
    /// значимы.
    tail: [u8; MAX_WRITE_SIZE],
    /// Сколько байт в `tail` — меньше слова.
    pending: usize,
}

impl Transfer {
    /// Начинает приём образа длиной `announced`: проверяет длину и готовит
    /// раздел.
    ///
    /// Порядок важен: длина сверяется с вместимостью ДО стирания, потому что
    /// стирание уничтожает образ, в который устройство откатывается. Узнать,
    /// что длина негодная, после этого — значит остаться без обеих прошивок.
    ///
    /// # Errors
    ///
    /// - `Empty` — заявлена нулевая длина,
    /// - `TooLong` — длина больше вместимости раздела,
    /// - `Flash` — флеш отказал.
    pub async fn start<D: NorFlash, S: NorFlash>(
        flash: &mut Updater<Partitions<D, S>>,
        announced: u32,
    ) -> Result<Self, DownloadError> {
        if announced == 0 {
            return Err(DownloadError::Empty);
        }

        let capacity = flash.capacity();
        if announced > capacity {
            return Err(DownloadError::TooLong {
                announced,
                capacity,
            });
        }

        // Слово — от 1 до `MAX_WRITE_SIZE`, и делить на него можно; раздел
        // кратен странице, страница — слову, так что и добитый до слова хвост
        // не выходит за раздел. Всё это проверил `Updater::new` при сборке.
        let word = flash.write_granularity();

        flash
            .prepare(announced)
            .await
            .map_err(DownloadError::Flash)?;

        Ok(Self {
            announced,
            word,
            written: 0,
            tail: [0; MAX_WRITE_SIZE],
            pending: 0,
        })
    }

    /// Сколько байт образа принято — записано и ждёт в хвосте.
    #[must_use]
    pub fn received(&self) -> u32 {
        self.written + self.pending_u32()
    }

    /// Принимает кусок по смещению `offset` и возвращает, сколько байт принято
    /// всего.
    ///
    /// Кусок, целиком лежащий в уже принятом, — повтор (ответ на него
    /// потерялся, и отправитель послал снова): подтверждается без записи.
    /// Во флеш уходят только целые слова; остаток куска ждёт следующего.
    /// После ошибки приём продолжать нельзя: хвост и смещение уже не
    /// сходятся с тем, что во флеше, — узел бросает его.
    ///
    /// # Errors
    ///
    /// - `OutOfOrder` — кусок не встык к принятым: разрыв или наложение,
    /// - `TooMuchData` — кусок выводит образ за обещанную длину,
    /// - `Flash` — флеш отказал.
    ///
    /// # Panics
    ///
    /// Если кусок длиннее `u32::MAX`: размеры образов ограничены разделом
    /// флеша, на практике это невозможно.
    pub async fn chunk<D: NorFlash, S: NorFlash>(
        &mut self,
        flash: &mut Updater<Partitions<D, S>>,
        offset: u32,
        mut chunk: &[u8],
    ) -> Result<u32, DownloadError> {
        let len = u32::try_from(chunk.len()).expect("кусок короче u32::MAX байт");
        let expected = self.received();
        if offset != expected {
            return if offset.saturating_add(len) <= expected {
                Ok(expected)
            } else {
                Err(DownloadError::OutOfOrder { expected, offset })
            };
        }
        let received = expected.saturating_add(len);
        if received > self.announced {
            return Err(DownloadError::TooMuchData {
                announced: self.announced,
                received,
            });
        }

        let word_len = self.word as usize;
        // Сначала добить хвост прошлого куска: пока он неполон, писать во флеш
        // нечего, а порядок байт в образе обязан сохраниться.
        if self.pending > 0 {
            let take = (word_len - self.pending).min(chunk.len());
            self.tail[self.pending..self.pending + take].copy_from_slice(&chunk[..take]);
            self.pending += take;
            chunk = &chunk[take..];

            if self.pending < word_len {
                return Ok(received);
            }
            flash
                .write(self.written, &self.tail[..word_len])
                .await
                .map_err(DownloadError::Flash)?;
            self.written += self.word;
            self.pending = 0;
        }

        // Целые слова — прямо во флеш, без копирования через буфер.
        let whole = chunk.len() - chunk.len() % word_len;
        if whole > 0 {
            flash
                .write(self.written, &chunk[..whole])
                .await
                .map_err(DownloadError::Flash)?;
            self.written += u32::try_from(whole).expect("кусок короче u32::MAX байт");
        }

        // Остаток короче слова ждёт следующего куска.
        let rest = &chunk[whole..];
        self.tail[..rest.len()].copy_from_slice(rest);
        self.pending = rest.len();
        Ok(received)
    }

    /// Заканчивает приём и возвращает длину образа — её ждёт применение
    /// обновления.
    ///
    /// Последнее неполное слово добивается единицами — фиксированным
    /// значением, и только поэтому: называть `0xFF` «стёртым состоянием
    /// флеша» нельзя, на L0/L1 оно нулевое (docs/flash.md), и там добор — самая
    /// обычная запись.
    ///
    /// # Errors
    ///
    /// - `Incomplete` — принято меньше обещанного,
    /// - `Flash` — флеш отказал.
    pub async fn finish<D: NorFlash, S: NorFlash>(
        mut self,
        flash: &mut Updater<Partitions<D, S>>,
    ) -> Result<u32, DownloadError> {
        let received = self.received();
        if received != self.announced {
            return Err(DownloadError::Incomplete {
                announced: self.announced,
                received,
            });
        }

        if self.pending > 0 {
            let word_len = self.word as usize;
            self.tail[self.pending..word_len].fill(0xFF);
            flash
                .write(self.written, &self.tail[..word_len])
                .await
                .map_err(DownloadError::Flash)?;
        }
        Ok(self.announced)
    }

    /// `pending` в `u32`: он меньше слова, то есть не больше 32.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "pending меньше слова флеша (не больше 32 байт)"
    )]
    fn pending_u32(&self) -> u32 {
        self.pending as u32
    }
}

/// Что может пойти не так при приёме образа ([`Transfer`]).
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum DownloadError {
    /// Обещали образ нулевой длины. Проверяется здесь, а не оставляется
    /// реализации порта: `prepare(0)` не стёр бы ничего, и первая же запись
    /// легла бы в нестёртую память, на F2/F4/F7 молча.
    Empty,
    /// Устройство столько не примет: образ длиннее того, что помещается в
    /// раздел. Проверяется до стирания — раздел остаётся нетронутым.
    TooLong { announced: u32, capacity: u32 },
    /// Прислали больше, чем обещали в начале. Приём прекращается: это либо
    /// сбой канала, либо попытка вылезти за раздел.
    TooMuchData { announced: u32, received: u32 },
    /// Кусок не встык к принятым: разрыв или наложение. Повтор уже принятого
    /// куска сюда не попадает — его подтверждают без записи.
    OutOfOrder { expected: u32, offset: u32 },
    /// Передача кончилась, не добрав до обещанной длины.
    Incomplete { announced: u32, received: u32 },
    /// Отказ самого флеша.
    Flash(FlashError),
}

#[cfg(test)]
#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
mod tests {
    use embedded_storage::nor_flash::NorFlashErrorKind;
    use embedded_storage_async::nor_flash::NorFlash;
    use ports::{MAX_WRITE_SIZE, Partitions};

    use super::{DownloadError, Transfer};
    use crate::ota::fakes::{flash, flash_with_word};
    use crate::ota::{FlashError, Updater};

    /// Исход приёма целиком и сколько кусков в него ушло.
    struct Received {
        result: Result<u32, DownloadError>,
        taken: usize,
    }

    /// Приём целиком, как его ведёт узел: `start`, куски по одному, `finish`.
    /// Первая ошибка прекращает приём.
    async fn receive<D: NorFlash, S: NorFlash>(
        flash: &mut Updater<Partitions<D, S>>,
        announced: u32,
        chunks: impl IntoIterator<Item = Vec<u8>>,
    ) -> Received {
        let mut transfer = match Transfer::start(flash, announced).await {
            Ok(transfer) => transfer,
            Err(err) => {
                return Received {
                    result: Err(err),
                    taken: 0,
                };
            }
        };
        let mut taken = 0;
        let mut offset = 0;
        for chunk in chunks {
            taken += 1;
            let at = offset;
            offset += u32::try_from(chunk.len()).unwrap();
            if let Err(err) = transfer.chunk(flash, at, &chunk).await {
                return Received {
                    result: Err(err),
                    taken,
                };
            }
        }
        Received {
            result: transfer.finish(flash).await,
            taken,
        }
    }

    fn image(len: u32, seed: u32) -> Vec<u8> {
        (0..len).map(|i| (i * seed) as u8).collect()
    }

    /// Принимает `data` кусками `chunks` над разделом со словом `W` и
    /// отдаёт принятую длину и содержимое раздела.
    fn reassemble<const W: usize>(data: &[u8], chunks: Vec<Vec<u8>>) -> (u32, Vec<u8>) {
        let mut flash = flash_with_word::<W>(1024);
        let len = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("рантайм")
            .block_on(receive(&mut flash, data.len() as u32, chunks))
            .result
            .expect("приём");
        (len, flash.memory().clone())
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
            // Слово флеша — константа типа раздела: по экземпляру на ширину.
            let (len, memory) = match word {
                1 => reassemble::<1>(&data, chunks),
                2 => reassemble::<2>(&data, chunks),
                4 => reassemble::<4>(&data, chunks),
                8 => reassemble::<8>(&data, chunks),
                16 => reassemble::<16>(&data, chunks),
                _ => reassemble::<32>(&data, chunks),
            };

            proptest::prop_assert_eq!(len, data.len() as u32);
            proptest::prop_assert_eq!(&memory[..data.len()], &data[..]);
            let padded = data.len().next_multiple_of(word as usize);
            proptest::prop_assert!(memory[data.len()..padded].iter().all(|b| *b == 0xFF));
        }
    }

    /// Принять образ кусками по одному байту — самый недружелюбный случай,
    /// какой может выдать канал.
    #[tokio::test]
    async fn reassembles_an_image_from_single_byte_chunks() {
        let image = image(100, 1);
        let chunks = image.iter().map(|b| vec![*b]);
        let mut flash = flash(1024);

        let len = receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём должен завершиться");

        assert_eq!(len, image.len() as u32);
        assert_eq!(&flash.memory()[..image.len()], &image[..]);
    }

    /// Куски, не кратные ни слову, ни друг другу.
    #[tokio::test]
    async fn reassembles_an_image_from_ragged_chunks() {
        let image = image(200, 7);
        let chunks = image.chunks(13).map(<[u8]>::to_vec);
        let mut flash = flash(1024);

        receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём");

        assert_eq!(&flash.memory()[..image.len()], &image[..]);
        // Всё, что дошло до флеша, было кратно слову — иначе фейк отказал бы;
        // здесь же проверяется, что запись вообще шла словами, а не одним
        // куском в конце.
        assert!(
            flash.writes().len() > 1,
            "запись должна идти по мере приёма"
        );
    }

    /// Слово в 32 байта (самое широкое у STM32) и образ, не кратный ему.
    #[tokio::test]
    async fn pads_the_last_word_of_an_unaligned_image() {
        let image = image(70, 1);
        let chunks = vec![image.clone()];
        let mut flash = flash_with_word::<MAX_WRITE_SIZE>(1024);

        receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём");

        assert_eq!(&flash.memory()[..image.len()], &image[..]);
        // Хвост добит единицами — фиксированным значением, а не мусором.
        assert_eq!(&flash.memory()[image.len()..96], &[0xFF; 26][..]);
    }

    /// Нулевая длина отвергается здесь, а не оставляется реализации порта:
    /// `prepare(0)` не стёр бы ничего, и первая же запись легла бы в нестёртую
    /// память, на F2/F4/F7 молча.
    #[tokio::test]
    async fn refuses_an_empty_image_before_touching_the_partition() {
        let chunks = Vec::new();
        let mut flash = flash(128);
        flash.memory().fill(0xA5);

        let refused = receive(&mut flash, 0, chunks).await.result;

        assert_eq!(refused, Err(DownloadError::Empty));
        assert!(!flash.prepared(), "раздел не должен быть подготовлен");
        assert!(
            flash.memory().iter().all(|b| *b == 0xA5),
            "раздел не должен быть стёрт"
        );
    }

    /// Образ длиннее раздела отвергается ДО стирания: иначе устройство
    /// осталось бы и без нового образа, и без того, куда откатываться.
    #[tokio::test]
    async fn refuses_an_image_longer_than_the_partition_without_erasing() {
        let chunks = Vec::new();
        let mut flash = flash(128);
        flash.memory().fill(0xA5);

        let refused = receive(&mut flash, 129, chunks).await.result;

        assert_eq!(
            refused,
            Err(DownloadError::TooLong {
                announced: 129,
                capacity: 128
            })
        );
        assert!(!flash.prepared());
        assert!(flash.memory().iter().all(|b| *b == 0xA5));
    }

    /// Канал прислал больше, чем обещал: приём прекращается на том куске, где
    /// это стало видно, — канал дальше не читается.
    #[tokio::test]
    async fn refuses_more_data_than_announced() {
        let chunks = vec![vec![0; 16], vec![0; 1], vec![0; 100]];
        let mut flash = flash(1024);

        let received = receive(&mut flash, 16, chunks).await;
        let refused = received.result;

        assert_eq!(
            refused,
            Err(DownloadError::TooMuchData {
                announced: 16,
                received: 17
            })
        );
        assert_eq!(received.taken, 2, "после лишнего куска приём прекращается");
    }

    /// Повтор уже принятого куска (ответ на него потерялся) подтверждается
    /// без записи: образ не сдвигается, приём идёт дальше.
    #[tokio::test]
    async fn a_repeated_chunk_is_acknowledged_without_writing() {
        let image = image(16, 5);
        let mut flash = flash(128);
        let mut transfer = Transfer::start(&mut flash, 16).await.unwrap();

        assert_eq!(transfer.chunk(&mut flash, 0, &image[..8]).await, Ok(8));
        let writes = flash.writes().len();
        assert_eq!(
            transfer.chunk(&mut flash, 0, &image[..8]).await,
            Ok(8),
            "повтор"
        );
        assert_eq!(flash.writes().len(), writes, "повтор не пишется");
        assert_eq!(transfer.chunk(&mut flash, 8, &image[8..]).await, Ok(16));
        assert_eq!(transfer.finish(&mut flash).await, Ok(16));
        assert_eq!(&flash.memory()[..16], &image[..]);
    }

    /// Разрыв и наложение — отказ: кусок не встык к принятым.
    #[tokio::test]
    async fn a_gap_or_an_overlap_is_out_of_order() {
        let mut flash = flash(128);
        let mut transfer = Transfer::start(&mut flash, 32).await.unwrap();
        transfer.chunk(&mut flash, 0, &[0; 8]).await.unwrap();

        assert_eq!(
            transfer.chunk(&mut flash, 16, &[0; 8]).await,
            Err(DownloadError::OutOfOrder {
                expected: 8,
                offset: 16
            }),
            "разрыв"
        );
        assert_eq!(
            transfer.chunk(&mut flash, 4, &[0; 8]).await,
            Err(DownloadError::OutOfOrder {
                expected: 8,
                offset: 4
            }),
            "наложение"
        );
    }

    /// Передача оборвалась (канал ответил «конец» раньше времени): делать вид,
    /// что всё в порядке, нельзя.
    #[tokio::test]
    async fn refuses_an_incomplete_transfer() {
        let chunks = vec![vec![0; 20]];
        let mut flash = flash(1024);

        let refused = receive(&mut flash, 32, chunks).await.result;

        assert_eq!(
            refused,
            Err(DownloadError::Incomplete {
                announced: 32,
                received: 20
            })
        );
    }

    /// Отказ флеша доходит до вызывающего как есть, а не теряется.
    #[tokio::test]
    async fn passes_a_flash_failure_through() {
        let chunks = vec![vec![0; 8]];
        let mut flash = flash(1024);
        flash.refuse_writes_from(0);

        let failed = receive(&mut flash, 8, chunks).await.result;

        assert_eq!(
            failed,
            Err(DownloadError::Flash(FlashError::Flash(
                NorFlashErrorKind::Other
            )))
        );
    }
}
