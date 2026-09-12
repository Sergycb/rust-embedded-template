//! Помощники host-тестов: прогон future без исполнителя и фейки обоих портов
//! обновления.
//!
//! Один фейк на порт, общий для `download`, `update` и `ota`: сценарии у
//! модулей разные, а придирки фейка — те же, что у настоящего адаптера
//! (кратность записи слову, запись только после `prepare`), и держать их в
//! трёх копиях значило бы проверять три разных флеша.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use ports::{FirmwareUpdate, ImageSource, SignedFirmwareUpdate, VerifyError};

use crate::firmware::pack;
use crate::update::VERSION_BYTES;

/// Ключ, которым «подписаны» образы в тестах. Ненулевой: нули — «ключ не
/// подставлен», и логика отказывает до криптографии.
pub(crate) const KEY: [u8; 32] = [7; 32];
/// Подпись, которую тесты кладут в заголовок; фейк её не проверяет, а
/// записывает — важно, что до него доехала именно она.
pub(crate) const SIGNATURE: [u8; 64] = [9; 64];

/// Крутит future фейков в host-тестах: ни один порт-фейк не ждёт по-настоящему,
/// поэтому исполнитель здесь не нужен — достаточно одного `poll`.
pub(crate) fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("фейки портов не ждут: future не должна возвращать Pending"),
    }
}

/// Канал, отдающий заранее нарезанные куски; на `fail_at`-м вызове `next`
/// отказывает — так разыгрывается обрыв связи.
pub(crate) struct FakeLink {
    chunks: Vec<Vec<u8>>,
    /// Сколько кусков уже отдано — по нему тест видит, читался ли канал.
    pub(crate) next: usize,
    pub(crate) fail_at: Option<usize>,
}

impl FakeLink {
    pub(crate) fn of(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            next: 0,
            fail_at: None,
        }
    }
}

impl ImageSource for FakeLink {
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

/// Флеш в памяти, придирчивый ровно там же, где настоящий, плюс всё, что
/// нужно применению с подписью: версия, ключ, исход проверки.
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
    pub(crate) version: u32,
    pub(crate) key: [u8; 32],
    pub(crate) verify: Result<(), VerifyError<&'static str>>,
    pub(crate) verified: Vec<([u8; 64], u32)>,
    /// Что ответить на `mark_updated`; `Err` разыгрывает отказ флеша.
    pub(crate) mark_updated: Result<(), &'static str>,
    /// Сколько раз просили обмен разделов.
    pub(crate) updated: usize,
}

impl FakeFlash {
    pub(crate) fn new(capacity: usize, word: u32) -> Self {
        Self {
            memory: vec![0xFF; capacity],
            capacity: capacity as u32,
            word,
            busy: false,
            prepared: None,
            writes: Vec::new(),
            version: pack(1, 2, 3),
            key: KEY,
            verify: Ok(()),
            verified: Vec::new(),
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

impl FirmwareUpdate for FakeFlash {
    type Error = &'static str;

    fn write_granularity(&mut self) -> u32 {
        self.word
    }

    fn capacity(&mut self) -> Result<u32, Self::Error> {
        Ok(self.capacity)
    }

    fn is_busy(&mut self) -> Result<bool, Self::Error> {
        Ok(self.busy)
    }

    fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
        if len == 0 || len > self.capacity {
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
        self.updated += 1;
        self.mark_updated
    }
}

impl SignedFirmwareUpdate for FakeFlash {
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
    ) -> Result<(), VerifyError<Self::Error>> {
        self.verified.push((*signature, len));
        self.verify
    }
}
