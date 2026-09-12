//! Приём образа по кускам: всё, что одинаково для любого канала доставки.
//!
//! Канал у каждой платы свой — USB CDC, UART, сеть, SD-карта, — и шаблон его не
//! выбирает: канал — это порт [`ImageSource`], который реализует проект. Но
//! вокруг канала есть работа, которая от него не зависит вовсе, и делать её в
//! каждом проекте заново незачем:
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
//! [`receive`] делает всё перечисленное:
//!
//! ```ignore
//! let len = domain::download::receive(&mut link, &mut board.ota, announced_len).await?;
//! // дальше — применение обновления: `domain::update::apply_signed` или
//! // `board.ota.mark_updated()`, смотря выбрана ли подпись при генерации
//! ```
//!
//! Функция, а не структура с состоянием, по общему правилу проекта: домен —
//! логика над портами, а всё состояние приёма (сколько принято, хвост до
//! слова) живёт в локальных переменных одного вызова. Здесь, а не в `bsp`:
//! это правила приёма, а не свойство флеша, и проверяются они на хосте — с
//! фейками обоих портов, на которых можно разыграть и обрыв связи, и лишний
//! байт, и кусок в один байт.

use ports::{DownloadError, FirmwareUpdate, ImageSource};

/// Самое большое слово программирования среди STM32 — тридцать два байта (H7).
///
/// Буфер фиксированного размера, а не `const N: usize`: гранулярность известна
/// только в рантайме (её отдаёт порт), а тащить её в тип значило бы протащить
/// параметр через весь код приёма ради экономии двух десятков байт RAM.
pub const MAX_WORD: usize = 32;

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
    if word == 0 || word as usize > MAX_WORD {
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

    flash.prepare(announced).map_err(DownloadError::Flash)?;

    let mut written: u32 = 0;
    // Хвост, не добравший до целого слова. Первые `pending` байт значимы.
    let mut tail = [0u8; MAX_WORD];
    let mut pending: usize = 0;

    while let Some(mut chunk) = source.next().await.map_err(DownloadError::Source)? {
        let received = (written + pending as u32).saturating_add(chunk.len() as u32);
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
                .map_err(DownloadError::Flash)?;
            written += whole as u32;
        }

        // Остаток короче слова ждёт следующего куска.
        let rest = &chunk[whole..];
        tail[..rest.len()].copy_from_slice(rest);
        pending = rest.len();
    }

    let received = written + pending as u32;
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
            .map_err(DownloadError::Flash)?;
    }
    Ok(announced)
}

#[cfg(test)]
mod tests {
    use super::{MAX_WORD, receive};
    use crate::test_support::block_on;
    use ports::{DownloadError, FirmwareUpdate, ImageSource};

    /// Канал, отдающий заранее нарезанные куски; на `fail_at`-м вызове
    /// отказывает — так разыгрывается обрыв связи.
    struct Chunks {
        chunks: Vec<Vec<u8>>,
        next: usize,
        fail_at: Option<usize>,
    }

    impl Chunks {
        fn of(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                chunks: chunks.into_iter().collect(),
                next: 0,
                fail_at: None,
            }
        }
    }

    impl ImageSource for Chunks {
        type Error = &'static str;

        async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error> {
            if self.fail_at == Some(self.next) {
                return Err("обрыв канала");
            }
            let i = self.next;
            self.next += 1;
            Ok(self.chunks.get(i).map(Vec::as_slice))
        }
    }

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

        fn is_busy(&mut self) -> Result<bool, Self::Error> {
            Ok(false)
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

        fn mark_updated(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn image(len: u32, seed: u32) -> Vec<u8> {
        (0..len).map(|i| (i * seed) as u8).collect()
    }

    /// Принять образ кусками по одному байту — самый недружелюбный случай,
    /// какой может выдать канал.
    #[test]
    fn reassembles_an_image_from_single_byte_chunks() {
        let image = image(100, 1);
        let mut source = Chunks::of(image.iter().map(|b| vec![*b]));
        let mut flash = FakeFlash::new(1024, 8);

        let len = block_on(receive(&mut source, &mut flash, image.len() as u32))
            .expect("приём должен завершиться");

        assert_eq!(len, image.len() as u32);
        assert_eq!(&flash.memory[..image.len()], &image[..]);
    }

    /// Куски, не кратные ни слову, ни друг другу.
    #[test]
    fn reassembles_an_image_from_ragged_chunks() {
        let image = image(200, 7);
        let mut source = Chunks::of(image.chunks(13).map(<[u8]>::to_vec));
        let mut flash = FakeFlash::new(1024, 8);

        block_on(receive(&mut source, &mut flash, image.len() as u32)).expect("приём");

        assert_eq!(&flash.memory[..image.len()], &image[..]);
        // Всё, что дошло до флеша, было кратно слову — иначе фейк отказал бы;
        // здесь же проверяется, что запись вообще шла словами, а не одним
        // куском в конце.
        assert!(flash.writes.len() > 1, "запись должна идти по мере приёма");
    }

    /// Слово в 32 байта (самое широкое у STM32) и образ, не кратный ему.
    #[test]
    fn pads_the_last_word_of_an_unaligned_image() {
        let image = image(70, 1);
        let mut source = Chunks::of([image.clone()]);
        let mut flash = FakeFlash::new(1024, MAX_WORD as u32);

        block_on(receive(&mut source, &mut flash, image.len() as u32)).expect("приём");

        assert_eq!(&flash.memory[..image.len()], &image[..]);
        // Хвост добит единицами — фиксированным значением, а не мусором.
        assert_eq!(&flash.memory[image.len()..96], &[0xFF; 26][..]);
    }

    /// Нулевая длина отвергается здесь, а не оставляется реализации порта:
    /// `prepare(0)` не стёр бы ничего, и первая же запись легла бы в нестёртую
    /// память, на F2/F4/F7 молча.
    #[test]
    fn refuses_an_empty_image_before_touching_the_partition() {
        let mut source = Chunks::of([]);
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = block_on(receive(&mut source, &mut flash, 0));

        assert_eq!(refused, Err(DownloadError::Empty));
        assert_eq!(flash.prepared, None, "раздел не должен быть подготовлен");
        assert!(
            flash.memory.iter().all(|b| *b == 0xA5),
            "раздел не должен быть стёрт"
        );
    }

    /// Образ длиннее раздела отвергается ДО стирания: иначе устройство
    /// осталось бы и без нового образа, и без того, куда откатываться.
    #[test]
    fn refuses_an_image_longer_than_the_partition_without_erasing() {
        let mut source = Chunks::of([]);
        let mut flash = FakeFlash::new(64, 8);
        flash.memory.fill(0xA5);

        let refused = block_on(receive(&mut source, &mut flash, 65));

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
    #[test]
    fn refuses_more_data_than_announced() {
        let mut source = Chunks::of([vec![0; 16], vec![0; 1], vec![0; 100]]);
        let mut flash = FakeFlash::new(1024, 8);

        let refused = block_on(receive(&mut source, &mut flash, 16));

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
    #[test]
    fn refuses_an_incomplete_transfer() {
        let mut source = Chunks::of([vec![0; 20]]);
        let mut flash = FakeFlash::new(1024, 8);

        let refused = block_on(receive(&mut source, &mut flash, 32));

        assert_eq!(
            refused,
            Err(DownloadError::Incomplete {
                announced: 32,
                received: 20
            })
        );
    }

    /// Гранулярность, с которой работать нельзя, отвергается до стирания.
    #[test]
    fn refuses_an_unusable_write_granularity() {
        for word in [0, MAX_WORD as u32 + 1] {
            let mut source = Chunks::of([]);
            let mut flash = FakeFlash::new(1024, word.max(1));
            flash.word = word;

            let refused = block_on(receive(&mut source, &mut flash, 32));

            assert_eq!(refused, Err(DownloadError::UnusableGranularity(word)));
            assert_eq!(flash.prepared, None);
        }
    }

    /// Образ, чья добитая до слова длина не влезает, отвергается тоже: иначе
    /// последнее слово ушло бы за границу раздела.
    #[test]
    fn refuses_an_image_whose_padded_length_overflows() {
        let mut source = Chunks::of([]);
        let mut flash = FakeFlash::new(100, 32);

        let refused = block_on(receive(&mut source, &mut flash, 100));

        assert_eq!(
            refused,
            Err(DownloadError::TooLong {
                announced: 100,
                capacity: 100
            })
        );
    }

    /// Отказ флеша доходит до вызывающего как есть, а не теряется.
    #[test]
    fn passes_a_flash_failure_through() {
        let mut source = Chunks::of([vec![0; 8]]);
        // Обёртка, у которой `prepare` «проходит», но флаг подготовки не
        // ставит: первая же запись фейка ответит отказом, и он обязан дойти
        // до вызывающего как `Flash(_)`.
        struct NoPrepare(FakeFlash);
        impl FirmwareUpdate for NoPrepare {
            type Error = &'static str;
            fn write_granularity(&mut self) -> u32 {
                self.0.write_granularity()
            }
            fn capacity(&mut self) -> Result<u32, Self::Error> {
                self.0.capacity()
            }
            fn is_busy(&mut self) -> Result<bool, Self::Error> {
                self.0.is_busy()
            }
            fn prepare(&mut self, _len: u32) -> Result<(), Self::Error> {
                Ok(()) // «подготовил», но флаг не поставил — запись откажет
            }
            fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
                self.0.write(offset, data)
            }
            fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
                self.0.read(offset, buf)
            }
            fn mark_booted(&mut self) -> Result<(), Self::Error> {
                self.0.mark_booted()
            }
            fn mark_updated(&mut self) -> Result<(), Self::Error> {
                self.0.mark_updated()
            }
        }
        let mut flash = NoPrepare(FakeFlash::new(1024, 8));

        let failed = block_on(receive(&mut source, &mut flash, 8));

        assert_eq!(
            failed,
            Err(DownloadError::Flash("запись без подготовки раздела"))
        );
    }

    /// Отказ канала — тоже, и своим вариантом: кто именно отказал, важнее
    /// удобства одного `From`.
    #[test]
    fn passes_a_source_failure_through() {
        let mut source = Chunks::of([vec![0; 8], vec![0; 8]]);
        source.fail_at = Some(1);
        let mut flash = FakeFlash::new(1024, 8);

        let failed = block_on(receive(&mut source, &mut flash, 16));

        assert_eq!(failed, Err(DownloadError::Source("обрыв канала")));
    }
}
