//! Фейки портов обновления для host-тестов.
//!
//! Один фейк на порт, общий для `download`, `update` и `ota`: сценарии у
//! модулей разные, а придирки фейка — те же, что у настоящего адаптера
//! (кратность записи слову, запись только после `prepare`), и держать их в
//! трёх копиях значило бы проверять три разных флеша.
//!
//! Ни один фейк не ждёт по-настоящему — их future готовы с первого `poll`, —
//! поэтому тестам хватает однопоточного `#[tokio::test]`. Исключение —
//! `FakeFlash::confirm_takes`: подтверждение ждёт таймер `MockDriver`.

use embassy_time::{Duration, Timer};
use ports::{FirmwareUpdate, PendingImage};

use crate::ota::update::{LAYOUT_BYTES, VERSION_BYTES};

/// Отпечаток раскладки фейка — его отдаёт `layout()`; образы тестов несут
/// его в хвосте, кроме тех, что разыгрывают чужую раскладку.
pub(crate) const LAYOUT: u32 = 0x5EED_1A70;

/// Подпись, которую тесты кладут в `Start`; фейк её не проверяет, а
/// записывает — важно, что до него доехала именно она.
pub(super) const SIGNATURE: [u8; 64] = [9; 64];

/// Флеш в памяти, придирчивый ровно там же, где настоящий, плюс запасная
/// страница за `capacity` — туда применение с подписью кладёт запись для
/// bootloader'а.
///
/// Главное здесь — проверки кратности: без них тест не отличил бы
/// работающую буферизацию от её отсутствия, а на плате разница вылезла бы
/// отказом записи (или, на F2/F4/F7, испорченным образом).
pub(super) struct FakeFlash {
    pub(super) memory: Vec<u8>,
    /// То, что отдаёт `capacity()`. По умолчанию — длина `memory`; тесты
    /// применения ставят меньше, чтобы разыграть «в приёмник влез, в
    /// активный раздел — нет».
    pub(super) capacity: u32,
    pub(super) word: u32,
    pub(super) busy: bool,
    pub(super) prepared: Option<u32>,
    pub(super) writes: Vec<(u32, usize)>,
    /// Что ответить на запись в запасную страницу (запись для
    /// bootloader'а); `Err` разыгрывает отказ флеша.
    pub(super) record: Result<(), &'static str>,
    /// Что оставлено bootloader'у в запасной странице: подпись и длина.
    pub(super) recorded: Vec<([u8; 64], u32)>,
    /// Что ответить на `mark_updated`; `Err` разыгрывает отказ флеша.
    pub(super) mark_updated: Result<(), &'static str>,
    /// Сколько раз просили обмен разделов.
    pub(super) updated: usize,
    /// Что ответить на `mark_booted`; `Err` разыгрывает отказ флеша.
    pub(super) mark_booted: Result<(), &'static str>,
    /// Сколько раз образ подтвердили успешно.
    pub(super) booted: usize,
    /// Сколько раз звали `mark_booted` — удачно или нет.
    pub(super) confirm_attempts: usize,
    /// Сколько длится `mark_booted` — как стирание `STATE` на плате;
    /// `None` — мгновенно.
    pub(super) confirm_takes: Option<Duration>,
}

impl FakeFlash {
    /// Запасная страница за `capacity` — под запись для bootloader'а.
    const SPARE: usize = 128;

    pub(super) fn new(capacity: usize, word: u32) -> Self {
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
            mark_booted: Ok(()),
            booted: 0,
            confirm_attempts: 0,
            confirm_takes: None,
        }
    }

    /// Раздел, в котором уже лежит образ длиной `len` с отпечатком
    /// [`LAYOUT`] и версией `incoming` в хвосте, и вместимость меньше
    /// раздела — как у настоящей схемы с обменом.
    pub(super) fn with_image(len: u32, incoming: u32) -> Self {
        let mut flash = Self::new(256, 8);
        flash.capacity = 128;
        let version = (len - VERSION_BYTES) as usize;
        flash.memory[version - LAYOUT_BYTES as usize..version]
            .copy_from_slice(&LAYOUT.to_le_bytes());
        flash.memory[version..len as usize].copy_from_slice(&incoming.to_le_bytes());
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
            // ищет `domain::ota::boot::screen_update`, и целыми словами.
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

    fn layout(&self) -> u32 {
        LAYOUT
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
        self.confirm_attempts += 1;
        if self.mark_booted.is_ok() {
            self.booted += 1;
        }
        let (result, takes) = (self.mark_booted, self.confirm_takes);
        async move {
            if let Some(takes) = takes {
                Timer::after(takes).await;
            }
            result
        }
    }

    fn mark_updated(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        std::future::ready(self.do_mark_updated())
    }
}
