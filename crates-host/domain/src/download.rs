//! Приём образа по кускам: всё, что одинаково для любого канала доставки.
//!
//! Канал у каждой платы свой — USB CDC, UART, сеть, SD-карта, — и шаблон его не
//! выбирает. Но вокруг канала есть работа, которая от него не зависит вовсе, и
//! делать её в каждом проекте заново незачем:
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
//! [`Download`] делает всё перечисленное, а от вас ждёт только байты:
//!
//! ```ignore
//! use domain::download::Download;
//!
//! let mut download = Download::begin(&mut board.ota, announced_len)?;
//! while let Some(packet) = link.next().await {
//!     download.push(&mut board.ota, packet)?;
//! }
//! let len = download.finish(&mut board.ota)?;
//! // дальше — применение обновления, оно зависит от того, выбрана ли подпись
//! ```
//!
//! Здесь, а не в `bsp`, по общему правилу проекта: это правила приёма, а не
//! свойство флеша. Заодно они проверяются на хосте — с фейком порта, на котором
//! можно разыграть и обрыв связи, и лишний байт, и кусок в один байт.

use ports::FirmwareUpdate;

/// Самое большое слово программирования среди STM32 — тридцать два байта (H7).
///
/// Буфер фиксированного размера, а не `const N: usize`: гранулярность известна
/// только в рантайме (её отдаёт порт), а тащить её в тип значило бы протащить
/// параметр через весь код приёма ради экономии двух десятков байт RAM.
const MAX_WORD: usize = 32;

/// Что может пойти не так при приёме.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error<E> {
    /// Устройство столько не примет: образ длиннее того, что помещается в
    /// раздел. Проверяется до стирания — раздел остаётся нетронутым.
    TooLong { announced: u32, capacity: u32 },
    /// Прислали больше, чем обещали в начале. Приём прекращается: это либо
    /// сбой канала, либо попытка вылезти за раздел.
    TooMuchData { announced: u32, received: u32 },
    /// Передача кончилась, не добрав до обещанной длины.
    Incomplete { announced: u32, received: u32 },
    /// Порт назвал гранулярность записи, с которой работать нельзя: ноль
    /// (деление на него паникует, а паника в release — это сброс) или больше
    /// [`MAX_WORD`], то есть шире буфера. Ни того, ни другого у STM32 не
    /// бывает, но молча испортить образ хуже, чем отказать.
    UnusableGranularity(u32),
    /// Отказ самого флеша.
    Flash(E),
}

impl<E> From<E> for Error<E> {
    fn from(error: E) -> Self {
        Self::Flash(error)
    }
}

/// Приём одного образа: помнит, сколько уже принято, и копит хвост до слова.
pub struct Download {
    announced: u32,
    written: u32,
    word: u32,
    /// Хвост, не добравший до целого слова. Первые `pending` байт значимы.
    tail: [u8; MAX_WORD],
    pending: usize,
}

impl Download {
    /// Проверяет длину, готовит раздел и начинает приём.
    ///
    /// Порядок важен: длина сверяется с вместимостью ДО стирания, потому что
    /// стирание уничтожает образ, в который устройство откатывается. Узнать,
    /// что длина негодная, после этого — значит остаться без обеих прошивок.
    pub fn begin<F: FirmwareUpdate>(
        flash: &mut F,
        announced: u32,
    ) -> Result<Self, Error<F::Error>> {
        let capacity = flash.capacity()?;
        if announced > capacity {
            return Err(Error::TooLong {
                announced,
                capacity,
            });
        }

        // Ноль отвергается вместе со слишком широким словом, и это не
        // симметрия ради симметрии: на нуле первый же `push` поделил бы на
        // ноль, а деление паникует и в release — то есть уводит устройство в
        // сброс.
        let word = flash.write_granularity();
        if word == 0 || word as usize > MAX_WORD {
            return Err(Error::UnusableGranularity(word));
        }

        // Добитая до слова длина тоже обязана помещаться: `finish` пишет
        // последний кусок целым словом, и на образе, чья длина не кратна
        // слову, запись ушла бы за границу. У настоящего `Ota` это не
        // случается (раздел кратен слову), но порт такого не обещает.
        if announced.div_ceil(word).saturating_mul(word) > capacity {
            return Err(Error::TooLong {
                announced,
                capacity,
            });
        }

        flash.prepare(announced)?;
        Ok(Self {
            announced,
            written: 0,
            word,
            tail: [0; MAX_WORD],
            pending: 0,
        })
    }

    /// Принимает очередной кусок канала — любой длины, хоть в один байт.
    ///
    /// Во флеш уходит только то, что набралось на целые слова; остаток
    /// остаётся в буфере до следующего куска или до [`finish`](Self::finish).
    pub fn push<F: FirmwareUpdate>(
        &mut self,
        flash: &mut F,
        mut chunk: &[u8],
    ) -> Result<(), Error<F::Error>> {
        let received = self.received().saturating_add(chunk.len() as u32);
        if received > self.announced {
            return Err(Error::TooMuchData {
                announced: self.announced,
                received,
            });
        }

        // Сначала добить хвост прошлого куска: пока он неполон, писать во флеш
        // нечего, а порядок байт в образе обязан сохраниться.
        if self.pending > 0 {
            let need = self.word as usize - self.pending;
            let take = need.min(chunk.len());
            self.tail[self.pending..self.pending + take].copy_from_slice(&chunk[..take]);
            self.pending += take;
            chunk = &chunk[take..];

            if self.pending < self.word as usize {
                return Ok(());
            }
            let word = self.word;
            self.flush_tail(flash, word)?;
        }

        // Целые слова — прямо во флеш, без копирования через буфер.
        let whole = chunk.len() - chunk.len() % self.word as usize;
        if whole > 0 {
            flash.write(self.written, &chunk[..whole])?;
            self.written += whole as u32;
        }

        // Остаток короче слова ждёт следующего куска.
        let rest = &chunk[whole..];
        self.tail[..rest.len()].copy_from_slice(rest);
        self.pending = rest.len();
        Ok(())
    }

    /// Дописывает хвост и проверяет, что принято ровно столько, сколько
    /// обещали. Возвращает длину образа — её ждёт применение обновления.
    ///
    /// Последний кусок почти никогда не кратен слову, поэтому хвост добивается
    /// единицами. На образ это не влияет: устройство считает хеш по обещанной
    /// длине, а добитые байты в неё не входят.
    ///
    /// Единицами, а не нулями, — потому что значение должно быть
    /// фиксированным, и только поэтому. Называть `0xFF` «стёртым состоянием
    /// флеша» здесь нельзя: на L0/L1 и STM32WB10CC/WB15CC стёртое состояние
    /// нулевое (см. CLAUDE.md, «Стёртый флеш»), и на таких чипах добор —
    /// самая обычная запись, а не «ничего не менять».
    pub fn finish<F: FirmwareUpdate>(mut self, flash: &mut F) -> Result<u32, Error<F::Error>> {
        let received = self.received();
        if received != self.announced {
            return Err(Error::Incomplete {
                announced: self.announced,
                received,
            });
        }

        if self.pending > 0 {
            let word = self.word;
            self.tail[self.pending..word as usize].fill(0xFF);
            self.flush_tail(flash, word)?;
        }
        Ok(self.announced)
    }

    /// Сколько байт образа уже пришло — вместе с теми, что ждут в буфере.
    pub fn received(&self) -> u32 {
        self.written + self.pending as u32
    }

    fn flush_tail<F: FirmwareUpdate>(
        &mut self,
        flash: &mut F,
        word: u32,
    ) -> Result<(), Error<F::Error>> {
        flash.write(self.written, &self.tail[..word as usize])?;
        self.written += word;
        self.pending = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Download, Error, MAX_WORD};
    use ports::FirmwareUpdate;

    /// Флеш в памяти, придирчивый ровно там же, где настоящий.
    ///
    /// Главное здесь — проверки кратности: без них тест не отличил бы
    /// работающую буферизацию от её отсутствия, а на плате разница вылезла бы
    /// отказом записи (или, на F2/F4/F7, испорченным образом).
    struct FakeFlash {
        memory: Vec<u8>,
        word: u32,
        prepared: Option<u32>,
        writes: Vec<(u32, usize)>,
    }

    impl FakeFlash {
        fn new(capacity: usize, word: u32) -> Self {
            Self {
                memory: vec![0xFF; capacity],
                word,
                prepared: None,
                writes: Vec::new(),
            }
        }
    }

    impl FirmwareUpdate for FakeFlash {
        type Error = &'static str;

        fn write_granularity(&mut self) -> u32 {
            self.word
        }

        fn capacity(&mut self) -> Result<u32, Self::Error> {
            Ok(self.memory.len() as u32)
        }

        fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
            if len == 0 || len > self.memory.len() as u32 {
                return Err("негодная длина");
            }
            self.prepared = Some(len);
            self.memory.fill(0xFF);
            Ok(())
        }

        fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
            if self.prepared.is_none() {
                return Err("запись без подготовки раздела");
            }
            if !(data.len() as u32).is_multiple_of(self.word) {
                return Err("длина записи не кратна слову");
            }
            if !offset.is_multiple_of(self.word) {
                return Err("смещение не кратно слову");
            }
            let start = offset as usize;
            self.memory[start..start + data.len()].copy_from_slice(data);
            self.writes.push((offset, data.len()));
            Ok(())
        }

        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            buf.copy_from_slice(&self.memory[start..start + buf.len()]);
            Ok(())
        }

        fn mark_booted(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Принять образ кусками по одному байту — самый недружелюбный случай,
    /// какой может выдать канал.
    #[test]
    fn reassembles_an_image_from_single_byte_chunks() {
        let image: Vec<u8> = (0..100u32).map(|i| i as u8).collect();
        let mut flash = FakeFlash::new(1024, 8);

        let mut download =
            Download::begin(&mut flash, image.len() as u32).expect("приём должен начаться");
        for byte in &image {
            download
                .push(&mut flash, &[*byte])
                .expect("байт должен приниматься");
        }
        let len = download
            .finish(&mut flash)
            .expect("приём должен завершиться");

        assert_eq!(len, image.len() as u32);
        assert_eq!(&flash.memory[..image.len()], &image[..]);
    }

    /// Куски, не кратные ни слову, ни друг другу.
    #[test]
    fn reassembles_an_image_from_ragged_chunks() {
        let image: Vec<u8> = (0..200u32).map(|i| (i * 7) as u8).collect();
        let mut flash = FakeFlash::new(1024, 8);

        let mut download = Download::begin(&mut flash, image.len() as u32).expect("начало");
        for chunk in image.chunks(13) {
            download.push(&mut flash, chunk).expect("кусок");
        }
        download.finish(&mut flash).expect("конец");

        assert_eq!(&flash.memory[..image.len()], &image[..]);
        // Всё, что дошло до флеша, было кратно слову — иначе фейк отказал бы;
        // здесь же проверяется, что запись вообще шла словами, а не одним
        // куском в конце.
        assert!(flash.writes.len() > 1, "запись должна идти по мере приёма");
    }

    /// Слово в 32 байта (самое широкое у STM32) и образ, не кратный ему.
    #[test]
    fn pads_the_last_word_of_an_unaligned_image() {
        let image: Vec<u8> = (0..70u32).map(|i| i as u8).collect();
        let mut flash = FakeFlash::new(1024, MAX_WORD as u32);

        let mut download = Download::begin(&mut flash, image.len() as u32).expect("начало");
        download.push(&mut flash, &image).expect("кусок");
        download.finish(&mut flash).expect("конец");

        assert_eq!(&flash.memory[..image.len()], &image[..]);
        // Хвост добит единицами — фиксированным значением, а не мусором. Что
        // именно это значение, важно только тем, что оно одно и то же на
        // хосте и на устройстве: «стёртым состоянием» его называть нельзя,
        // на части чипов флеш стирается в ноль.
        assert_eq!(&flash.memory[image.len()..96], &[0xFF; 26][..]);
    }

    /// Образ длиннее раздела отвергается ДО стирания: иначе устройство
    /// осталось бы и без нового образа, и без того, куда откатываться.
    #[test]
    fn refuses_an_image_longer_than_the_partition_without_erasing() {
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = Download::begin(&mut flash, 65);

        assert_eq!(
            refused.err(),
            Some(Error::TooLong {
                announced: 65,
                capacity: 64
            })
        );
        assert_eq!(flash.prepared, None, "раздел не должен быть подготовлен");
        assert!(
            flash.memory.iter().all(|b| *b == 0xA5),
            "раздел не должен быть стёрт"
        );
    }

    /// Канал прислал больше, чем обещал: приём прекращается на том куске, где
    /// это стало видно.
    #[test]
    fn refuses_more_data_than_announced() {
        let mut flash = FakeFlash::new(1024, 8);
        let mut download = Download::begin(&mut flash, 16).expect("начало");

        download.push(&mut flash, &[0; 16]).expect("ровно столько");
        let refused = download.push(&mut flash, &[0; 1]);

        assert_eq!(
            refused,
            Err(Error::TooMuchData {
                announced: 16,
                received: 17
            })
        );
    }

    /// Передача оборвалась: `finish` не должен делать вид, что всё в порядке.
    #[test]
    fn refuses_to_finish_an_incomplete_transfer() {
        let mut flash = FakeFlash::new(1024, 8);
        let mut download = Download::begin(&mut flash, 32).expect("начало");
        download.push(&mut flash, &[0; 20]).expect("часть");

        let refused = download.finish(&mut flash);

        assert_eq!(
            refused,
            Err(Error::Incomplete {
                announced: 32,
                received: 20
            })
        );
    }

    /// Гранулярность, с которой работать нельзя, отвергается до стирания.
    ///
    /// Ноль здесь не теоретический: на нём первый же `push` делил бы на ноль,
    /// а деление паникует и в release — то есть уводит устройство в сброс.
    #[test]
    fn refuses_an_unusable_write_granularity() {
        for word in [0, MAX_WORD as u32 + 1] {
            let mut flash = FakeFlash::new(1024, word.max(1));
            flash.word = word;

            let refused = Download::begin(&mut flash, 32);

            assert_eq!(refused.err(), Some(Error::UnusableGranularity(word)));
            assert_eq!(flash.prepared, None, "раздел не должен быть подготовлен");
        }
    }

    /// Образ, чья добитая до слова длина не влезает, отвергается тоже: иначе
    /// последнее слово ушло бы за границу раздела.
    #[test]
    fn refuses_an_image_whose_padded_length_overflows() {
        let mut flash = FakeFlash::new(100, 32);

        let refused = Download::begin(&mut flash, 100);

        assert_eq!(
            refused.err(),
            Some(Error::TooLong {
                announced: 100,
                capacity: 100
            })
        );
    }

    /// Отказ флеша доходит до вызывающего как есть, а не теряется.
    #[test]
    fn passes_a_flash_failure_through() {
        let mut flash = FakeFlash::new(1024, 8);
        let mut download = Download::begin(&mut flash, 64).expect("начало");
        flash.prepared = None; // фейк начнёт отвечать «запись без подготовки»

        let failed = download.push(&mut flash, &[0; 8]);

        assert_eq!(failed, Err(Error::Flash("запись без подготовки раздела")));
    }

    /// Счётчик принятого учитывает и то, что ещё лежит в буфере: иначе
    /// прогресс замирал бы между целыми словами.
    #[test]
    fn counts_bytes_still_waiting_in_the_buffer() {
        let mut flash = FakeFlash::new(1024, 8);
        let mut download = Download::begin(&mut flash, 32).expect("начало");

        download.push(&mut flash, &[0; 3]).expect("меньше слова");

        assert_eq!(download.received(), 3);
    }
}
