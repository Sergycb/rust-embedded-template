//! Данные slave'а: трейт [`Registers`] и представления битов и регистров
//! кадра, через которые он их читает и пишет.

use crate::modbus::Exception;

/// Биты (coils, discrete inputs) в том виде, в каком они лежат в кадре:
/// упакованы по восемь, младший бит байта — младший адрес.
///
/// Представление, а не копия: биты читаются прямо из принятого кадра.
#[derive(Debug, Clone, Copy)]
pub struct Bits<'a> {
    bytes: &'a [u8],
    len: u16,
}

impl<'a> Bits<'a> {
    /// `len` бит, упакованных в `bytes`.
    ///
    /// # Panics
    ///
    /// Если `bytes` не ровно `len.div_ceil(8)` байт: размер кадра проверен
    /// до этого места, и несовпадение — ошибка вызывающего кода.
    #[must_use]
    pub fn new(bytes: &'a [u8], len: u16) -> Self {
        assert_eq!(
            bytes.len(),
            usize::from(len.div_ceil(8)),
            "{len} бит не упаковываются в {} байт",
            bytes.len()
        );
        Self { bytes, len }
    }

    /// Количество бит.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.len
    }

    /// Пуст ли диапазон.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Бит с номером `i` от начала диапазона.
    ///
    /// # Panics
    ///
    /// Если `i >= len()` — как индексация среза.
    #[must_use]
    pub fn get(&self, i: u16) -> bool {
        assert!(i < self.len, "бит {i} вне диапазона из {}", self.len);
        self.bytes[usize::from(i / 8)] & (1 << (i % 8)) != 0
    }

    /// Все биты по порядку адресов.
    pub fn iter(&self) -> impl Iterator<Item = bool> + '_ {
        (0..self.len).map(|i| self.get(i))
    }
}

/// Место под биты ответа — прямо в буфере кадра, упакованное так же, как
/// [`Bits`]. Все биты изначально сброшены.
#[derive(Debug)]
pub struct BitsMut<'a> {
    bytes: &'a mut [u8],
    len: u16,
}

impl<'a> BitsMut<'a> {
    /// `len` бит поверх `bytes`; содержимое `bytes` обнуляется.
    ///
    /// # Panics
    ///
    /// Если `bytes` не ровно `len.div_ceil(8)` байт — по той же причине, что
    /// у [`Bits::new`].
    #[must_use]
    pub fn new(bytes: &'a mut [u8], len: u16) -> Self {
        assert_eq!(
            bytes.len(),
            usize::from(len.div_ceil(8)),
            "{len} бит не упаковываются в {} байт",
            bytes.len()
        );
        bytes.fill(0);
        Self { bytes, len }
    }

    /// Количество бит.
    #[must_use]
    pub const fn len(&self) -> u16 {
        self.len
    }

    /// Пуст ли диапазон.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Ставит бит с номером `i` от начала диапазона.
    ///
    /// # Panics
    ///
    /// Если `i >= len()` — как индексация среза.
    pub fn set(&mut self, i: u16, value: bool) {
        assert!(i < self.len, "бит {i} вне диапазона из {}", self.len);
        let mask = 1 << (i % 8);
        let byte = &mut self.bytes[usize::from(i / 8)];
        if value {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
}

/// 16-битные регистры в том виде, в каком они лежат в кадре: старший байт
/// первым.
///
/// Представление, а не копия: значения читаются прямо из принятого кадра.
#[derive(Debug, Clone, Copy)]
pub struct Words<'a> {
    bytes: &'a [u8],
}

impl<'a> Words<'a> {
    /// Регистры поверх `bytes` — по два байта на регистр.
    ///
    /// # Panics
    ///
    /// Если длина `bytes` нечётная или регистров больше, чем считает `u16`:
    /// размер кадра проверен до этого места.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        assert!(
            bytes.len().is_multiple_of(2) && u16::try_from(bytes.len() / 2).is_ok(),
            "{} байт — не целое число регистров или больше 65535 регистров",
            bytes.len()
        );
        Self { bytes }
    }

    /// Количество регистров.
    #[must_use]
    pub fn len(&self) -> u16 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "new() не пропускает больше u16::MAX регистров"
        )]
        let len = (self.bytes.len() / 2) as u16;
        len
    }

    /// Пуст ли диапазон.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Регистр с номером `i` от начала диапазона.
    ///
    /// # Panics
    ///
    /// Если `i >= len()` — как индексация среза.
    #[must_use]
    pub fn get(&self, i: u16) -> u16 {
        let at = usize::from(i) * 2;
        u16::from_be_bytes([self.bytes[at], self.bytes[at + 1]])
    }

    /// Все регистры по порядку адресов.
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
    }
}

/// Место под регистры ответа — прямо в буфере кадра, в том же порядке
/// байт, что [`Words`]. Все регистры изначально нулевые.
#[derive(Debug)]
pub struct WordsMut<'a> {
    bytes: &'a mut [u8],
}

impl<'a> WordsMut<'a> {
    /// Регистры поверх `bytes`; содержимое `bytes` обнуляется.
    ///
    /// # Panics
    ///
    /// Если длина `bytes` нечётная или регистров больше, чем считает `u16`, —
    /// по той же причине, что у [`Words::new`].
    #[must_use]
    pub fn new(bytes: &'a mut [u8]) -> Self {
        assert!(
            bytes.len().is_multiple_of(2) && u16::try_from(bytes.len() / 2).is_ok(),
            "{} байт — не целое число регистров или больше 65535 регистров",
            bytes.len()
        );
        bytes.fill(0);
        Self { bytes }
    }

    /// Количество регистров.
    #[must_use]
    pub fn len(&self) -> u16 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "new() не пропускает больше u16::MAX регистров"
        )]
        let len = (self.bytes.len() / 2) as u16;
        len
    }

    /// Пуст ли диапазон.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Пишет регистр с номером `i` от начала диапазона.
    ///
    /// # Panics
    ///
    /// Если `i >= len()` — как индексация среза.
    pub fn set(&mut self, i: u16, value: u16) {
        let at = usize::from(i) * 2;
        self.bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
    }
}

/// Данные устройства, видимые по шине Modbus: четыре таблицы — coils,
/// discrete inputs, input registers, holding registers.
///
/// Адреса — номера с нуля, как на проводе (без «40001»). Каждый метод
/// получает весь диапазон запроса одним вызовом: многорегистровое значение
/// (32-битное в двух holding-регистрах) видно целиком, и на запись можно
/// реагировать сразу — применить уставку, отправить событие задаче,
/// сохранить во флеш. Диапазон вне своей карты устройство отвергает
/// [`Exception::IllegalDataAddress`], недопустимое значение —
/// [`Exception::IllegalDataValue`].
///
/// Запись одного регистра (FC06) и нескольких (FC16) приходит в один и тот
/// же [`write_holding`](Self::write_holding), одного coil'а (FC05) и
/// нескольких (FC15) — в [`write_coils`](Self::write_coils): у устройства
/// нет причин различать их.
///
/// Таблицу, которой у устройства нет, можно не реализовывать: по умолчанию
/// каждый метод отвечает [`Exception::IllegalFunction`].
pub trait Registers {
    /// Читает coils `start..start + out.len()` в `out` (FC01).
    ///
    /// # Errors
    ///
    /// Исключение для master'а: диапазон вне карты, отказ устройства.
    async fn read_coils(&mut self, start: u16, out: BitsMut<'_>) -> Result<(), Exception> {
        let _ = (start, out);
        Err(Exception::IllegalFunction)
    }

    /// Читает discrete inputs `start..start + out.len()` в `out` (FC02).
    ///
    /// # Errors
    ///
    /// Исключение для master'а: диапазон вне карты, отказ устройства.
    async fn read_discrete_inputs(
        &mut self,
        start: u16,
        out: BitsMut<'_>,
    ) -> Result<(), Exception> {
        let _ = (start, out);
        Err(Exception::IllegalFunction)
    }

    /// Читает holding registers `start..start + out.len()` в `out` (FC03).
    ///
    /// # Errors
    ///
    /// Исключение для master'а: диапазон вне карты, отказ устройства.
    async fn read_holding(&mut self, start: u16, out: WordsMut<'_>) -> Result<(), Exception> {
        let _ = (start, out);
        Err(Exception::IllegalFunction)
    }

    /// Читает input registers `start..start + out.len()` в `out` (FC04).
    ///
    /// # Errors
    ///
    /// Исключение для master'а: диапазон вне карты, отказ устройства.
    async fn read_input(&mut self, start: u16, out: WordsMut<'_>) -> Result<(), Exception> {
        let _ = (start, out);
        Err(Exception::IllegalFunction)
    }

    /// Пишет coils начиная со `start` (FC05 — один, FC15 — несколько).
    ///
    /// Отказ — до изменения чего-либо: master считает, что при исключении
    /// не записано ничего.
    ///
    /// # Errors
    ///
    /// Исключение для master'а: диапазон вне карты, недопустимое значение.
    async fn write_coils(&mut self, start: u16, values: Bits<'_>) -> Result<(), Exception> {
        let _ = (start, values);
        Err(Exception::IllegalFunction)
    }

    /// Пишет holding registers начиная со `start` (FC06 — один, FC16 —
    /// несколько).
    ///
    /// Отказ — до изменения чего-либо: master считает, что при исключении
    /// не записано ничего.
    ///
    /// # Errors
    ///
    /// Исключение для master'а: диапазон вне карты, недопустимое значение.
    async fn write_holding(&mut self, start: u16, values: Words<'_>) -> Result<(), Exception> {
        let _ = (start, values);
        Err(Exception::IllegalFunction)
    }
}
