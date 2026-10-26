//! Приём образа по кускам: всё, что одинаково для любого протокола.
//!
//! Образ приходит командами (`ports::OtaCommand`): узел протокола переводит
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
//! живёт там, а шаги — функции над портом `FirmwareUpdate`. Здесь, а не в
//! `bsp`: это правила приёма, а не свойство флеша, и проверяются они на
//! хосте — с фейком порта, на котором можно разыграть и лишний байт, и кусок в
//! один байт.

use ports::{DownloadError, FirmwareUpdate, MAX_WRITE_SIZE};

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
    /// Начинает приём образа длиной `announced`: проверяет длину и слово
    /// флеша и готовит раздел.
    ///
    /// Порядок важен: длина сверяется с вместимостью ДО стирания, потому что
    /// стирание уничтожает образ, в который устройство откатывается. Узнать,
    /// что длина негодная, после этого — значит остаться без обеих прошивок.
    ///
    /// # Errors
    ///
    /// - `Empty` — заявлена нулевая длина,
    /// - `TooLong` — длина больше вместимости раздела или добитая длина не влезает,
    /// - `UnusableGranularity` — порт вернул негодную гранулярность,
    /// - `Flash` — флеш отказал.
    pub async fn start<F: FirmwareUpdate>(
        flash: &mut F,
        announced: u32,
    ) -> Result<Self, DownloadError<F::Error>> {
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
    pub async fn chunk<F: FirmwareUpdate>(
        &mut self,
        flash: &mut F,
        offset: u32,
        mut chunk: &[u8],
    ) -> Result<u32, DownloadError<F::Error>> {
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
    pub async fn finish<F: FirmwareUpdate>(
        mut self,
        flash: &mut F,
    ) -> Result<u32, DownloadError<F::Error>> {
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

#[cfg(test)]
#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
mod tests {
    use ports::MAX_WRITE_SIZE;

    use super::Transfer;
    use crate::ota::fakes::FakeFlash;
    use ports::{DownloadError, FirmwareUpdate};

    /// Исход приёма целиком и сколько кусков в него ушло.
    struct Received<E> {
        result: Result<u32, DownloadError<E>>,
        taken: usize,
    }

    /// Приём целиком, как его ведёт узел: `start`, куски по одному, `finish`.
    /// Первая ошибка прекращает приём.
    async fn receive<F: FirmwareUpdate>(
        flash: &mut F,
        announced: u32,
        chunks: impl IntoIterator<Item = Vec<u8>>,
    ) -> Received<F::Error> {
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
            let mut flash = FakeFlash::new(1024, word);

            let len = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("рантайм")
                .block_on(receive(&mut flash, data.len() as u32, chunks))
                .result
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
        let chunks = image.iter().map(|b| vec![*b]);
        let mut flash = FakeFlash::new(1024, 8);

        let len = receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём должен завершиться");

        assert_eq!(len, image.len() as u32);
        assert_eq!(&flash.memory[..image.len()], &image[..]);
    }

    /// Куски, не кратные ни слову, ни друг другу.
    #[tokio::test]
    async fn reassembles_an_image_from_ragged_chunks() {
        let image = image(200, 7);
        let chunks = image.chunks(13).map(<[u8]>::to_vec);
        let mut flash = FakeFlash::new(1024, 8);

        receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
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
        let chunks = vec![image.clone()];
        let mut flash = FakeFlash::new(1024, MAX_WRITE_SIZE as u32);

        receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
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
        let chunks = Vec::new();
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = receive(&mut flash, 0, chunks).await.result;

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
        let chunks = Vec::new();
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = receive(&mut flash, 65, chunks).await.result;

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
        let chunks = vec![vec![0; 16], vec![0; 1], vec![0; 100]];
        let mut flash = FakeFlash::new(1024, 8);

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
        let mut flash = FakeFlash::new(64, 8);
        let mut transfer = Transfer::start(&mut flash, 16).await.unwrap();

        assert_eq!(transfer.chunk(&mut flash, 0, &image[..8]).await, Ok(8));
        let writes = flash.writes.len();
        assert_eq!(
            transfer.chunk(&mut flash, 0, &image[..8]).await,
            Ok(8),
            "повтор"
        );
        assert_eq!(flash.writes.len(), writes, "повтор не пишется");
        assert_eq!(transfer.chunk(&mut flash, 8, &image[8..]).await, Ok(16));
        assert_eq!(transfer.finish(&mut flash).await, Ok(16));
        assert_eq!(&flash.memory[..16], &image[..]);
    }

    /// Разрыв и наложение — отказ: кусок не встык к принятым.
    #[tokio::test]
    async fn a_gap_or_an_overlap_is_out_of_order() {
        let mut flash = FakeFlash::new(64, 8);
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
        let mut flash = FakeFlash::new(1024, 8);

        let refused = receive(&mut flash, 32, chunks).await.result;

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
            let chunks = Vec::new();
            let mut flash = FakeFlash::new(1024, word.max(1));
            flash.word = word;

            let refused = receive(&mut flash, 32, chunks).await.result;

            assert_eq!(refused, Err(DownloadError::UnusableGranularity(word)));
            assert_eq!(flash.prepared, None);
        }
    }

    /// Образ, чья добитая до слова длина не влезает, отвергается тоже: иначе
    /// последнее слово ушло бы за границу раздела.
    #[tokio::test]
    async fn refuses_an_image_whose_padded_length_overflows() {
        let chunks = Vec::new();
        let mut flash = FakeFlash::new(100, 32);

        let refused = receive(&mut flash, 100, chunks).await.result;

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
        let chunks = vec![vec![0; 8]];
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
            fn layout(&mut self) -> u32 {
                self.0.layout()
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

        let failed = receive(&mut flash, 8, chunks).await.result;

        assert_eq!(
            failed,
            Err(DownloadError::Flash("запись без подготовки раздела"))
        );
    }
}
