//! Фейки портов обновления для host-тестов.
//!
//! Один фейк на порт, общий для `download`, `update` и `ota`: сценарии у
//! модулей разные, а придирки фейка — те же, что у настоящего адаптера
//! (кратность записи слову, запись только после `prepare`), и держать их в
//! трёх копиях значило бы проверять три разных флеша.
//!
//! Ни один фейк не ждёт по-настоящему — их future готовы с первого `poll`, —
//! поэтому тестам хватает однопоточного `#[tokio::test]`.

use std::collections::VecDeque;

use ports::{Announce, FirmwareUpdate, ImageSource, PendingImage, Rejection};

use crate::ota::update::VERSION_BYTES;

/// Подпись, которую тесты кладут в заголовок; фейк её не проверяет, а
/// записывает — важно, что до него доехала именно она.
pub(crate) const SIGNATURE: [u8; 64] = [9; 64];

/// Канал, отдающий заранее нарезанные куски; на `fail_at`-м вызове `next`
/// отказывает — так разыгрывается обрыв связи. Заголовки — по одному на
/// цикл; когда они кончаются, `begin` отвечает отказом — единственный выход
/// из цикла узла в тесте.
pub(crate) struct FakeLink {
    announces: VecDeque<Announce>,
    chunks: Vec<Vec<u8>>,
    /// Сколько кусков уже отдано — по нему тест видит, читался ли канал.
    pub(crate) next: usize,
    pub(crate) fail_at: Option<usize>,
    /// Что ответить на `finish`; `Err` разыгрывает обрыв при отчёте.
    pub(crate) finish: Result<(), &'static str>,
    /// Всё, что узел сообщил отправителю, в порядке вызовов.
    pub(crate) finished: Vec<Result<(), Rejection>>,
}

impl FakeLink {
    pub(crate) fn of(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            announces: VecDeque::new(),
            chunks: chunks.into_iter().collect(),
            next: 0,
            fail_at: None,
            finish: Ok(()),
            finished: Vec::new(),
        }
    }

    /// Заголовки, которые `begin` отдаст по одному на цикл.
    pub(crate) fn announcing(mut self, announces: impl IntoIterator<Item = Announce>) -> Self {
        self.announces = announces.into_iter().collect();
        self
    }
}

impl ImageSource for FakeLink {
    type Error = &'static str;

    fn begin(&mut self) -> impl Future<Output = Result<Announce, Self::Error>> {
        std::future::ready(self.announces.pop_front().ok_or("канал закрыт"))
    }

    fn next(&mut self) -> impl Future<Output = Result<Option<&[u8]>, Self::Error>> {
        if self.fail_at == Some(self.next) {
            return std::future::ready(Err("обрыв канала"));
        }
        let i = self.next;
        self.next += 1;
        std::future::ready(Ok(self.chunks.get(i).map(Vec::as_slice)))
    }

    fn finish(
        &mut self,
        outcome: Result<(), Rejection>,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.finished.push(outcome);
        std::future::ready(self.finish)
    }
}

/// Флеш в памяти, придирчивый ровно там же, где настоящий, плюс запасная
/// страница за `capacity` — туда применение с подписью кладёт запись для
/// bootloader'а.
///
/// Главное здесь — проверки кратности: без них тест не отличил бы
/// работающую буферизацию от её отсутствия, а на плате разница вылезла бы
/// отказом записи (или, на F2/F4/F7, испорченным образом).
pub(crate) struct FakeFlash {
    pub(crate) memory: Vec<u8>,
    /// То, что отдаёт `capacity()`. По умолчанию — длина `memory`; тесты
    /// применения ставят меньше, чтобы разыграть «в приёмник влез, в
    /// активный раздел — нет».
    pub(crate) capacity: u32,
    pub(crate) word: u32,
    pub(crate) busy: bool,
    pub(crate) prepared: Option<u32>,
    pub(crate) writes: Vec<(u32, usize)>,
    /// Что ответить на запись в запасную страницу (запись для
    /// bootloader'а); `Err` разыгрывает отказ флеша.
    pub(crate) record: Result<(), &'static str>,
    /// Что оставлено bootloader'у в запасной странице: подпись и длина.
    pub(crate) recorded: Vec<([u8; 64], u32)>,
    /// Что ответить на `mark_updated`; `Err` разыгрывает отказ флеша.
    pub(crate) mark_updated: Result<(), &'static str>,
    /// Сколько раз просили обмен разделов.
    pub(crate) updated: usize,
}

impl FakeFlash {
    /// Запасная страница за `capacity` — под запись для bootloader'а.
    const SPARE: usize = 128;

    pub(crate) fn new(capacity: usize, word: u32) -> Self {
        Self {
            memory: vec![0xFF; capacity + Self::SPARE],
            capacity: u32::try_from(capacity).unwrap(),
            word,
            busy: false,
            prepared: None,
            writes: Vec::new(),
            record: Ok(()),
            recorded: Vec::new(),
            mark_updated: Ok(()),
            updated: 0,
        }
    }

    /// Раздел, в котором уже лежит образ длиной `len` с версией `incoming` в
    /// хвосте, и вместимость меньше раздела — как у настоящей схемы с обменом.
    pub(crate) fn with_image(len: u32, incoming: u32) -> Self {
        let mut flash = Self::new(256, 8);
        flash.capacity = 128;
        flash.memory[(len - VERSION_BYTES) as usize..len as usize]
            .copy_from_slice(&incoming.to_le_bytes());
        flash
    }
}

impl FakeFlash {
    fn do_prepare(&mut self, len: u32) -> Result<(), &'static str> {
        if len == 0 || len > self.capacity {
            return Err("негодная длина");
        }
        self.prepared = Some(len);
        self.memory.fill(0xFF);
        Ok(())
    }

    fn do_write(&mut self, offset: u32, data: &[u8]) -> Result<(), &'static str> {
        if offset >= self.capacity {
            // Запасная страница: запись для bootloader'а — ровно там, где её
            // ищет `adapters::ota::screen_update`, и целыми словами.
            if offset != self.capacity {
                return Err("запись для bootloader'а не по смещению capacity");
            }
            if !u32::try_from(data.len()).is_ok_and(|v| v.is_multiple_of(self.word)) {
                return Err("длина записи не кратна слову");
            }
            self.record?;
            let bytes: &[u8; PendingImage::LEN] =
                data.try_into().ok().ok_or("запись не той длины")?;
            let record = PendingImage::from_bytes(bytes).ok_or("не запись о подписи")?;
            self.recorded.push((record.signature, record.len));
            return Ok(());
        }
        if self.prepared.is_none() {
            return Err("запись без подготовки раздела");
        }
        if !u32::try_from(data.len()).is_ok_and(|v| v.is_multiple_of(self.word)) {
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

    fn do_mark_updated(&mut self) -> Result<(), &'static str> {
        self.mark_updated?;
        self.updated += 1;
        Ok(())
    }
}

impl FirmwareUpdate for FakeFlash {
    type Error = &'static str;

    fn write_granularity(&mut self) -> u32 {
        self.word
    }

    fn capacity(&mut self) -> Result<u32, Self::Error> {
        Ok(self.capacity)
    }

    fn is_busy(&mut self) -> impl Future<Output = Result<bool, Self::Error>> {
        std::future::ready(Ok(self.busy))
    }

    fn prepare(&mut self, len: u32) -> impl Future<Output = Result<(), Self::Error>> {
        std::future::ready(self.do_prepare(len))
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> impl Future<Output = Result<(), Self::Error>> {
        std::future::ready(self.do_write(offset, data))
    }

    fn read(
        &mut self,
        offset: u32,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        let start = offset as usize;
        buf.copy_from_slice(&self.memory[start..start + buf.len()]);
        std::future::ready(Ok(()))
    }

    fn mark_booted(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        std::future::ready(Ok(()))
    }

    fn mark_updated(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        std::future::ready(self.do_mark_updated())
    }
}
