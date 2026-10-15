//! Порты Modbus: данные slave'а, транспорты master'а и slave'а, ошибки обмена.
//!
//! [`Registers`](crate::Registers) реализует приложение — это его данные,
//! которые видны по шине; пользуется им `domain::modbus::slave::serve`.
//! Транспорты — [`ModbusClient`](crate::ModbusClient) и
//! [`ModbusServer`](crate::ModbusServer) — реализуют `adapters::modbus`
//! (RTU, TCP, UDP). Ошибки — у `domain::modbus::slave`
//! ([`ServeError`](crate::modbus::ServeError)) и `domain::modbus::master`
//! ([`MasterError`](crate::modbus::MasterError)).

/// Наибольший PDU Modbus — функция и данные: 253 байта. Предел пришёл из
/// кадра RTU (256 байт с адресом и CRC) и одинаков для всех транспортов.
/// Буферы PDU у [`ModbusClient`] и [`ModbusServer`] — не короче.
pub const MAX_PDU: usize = 253;

/// Код исключения Modbus — ответ slave'а вместо данных.
///
/// Slave из [`Registers`] возвращает обычно первые четыре; master получает
/// любой, включая коды шлюза и нестандартные ([`Exception::Other`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Exception {
    /// 0x01: функция не поддерживается.
    IllegalFunction,
    /// 0x02: адрес или диапазон адресов вне карты устройства.
    IllegalDataAddress,
    /// 0x03: недопустимое значение (количество, значение coil'а, уставка).
    IllegalDataValue,
    /// 0x04: неисправимая ошибка устройства при выполнении запроса.
    ServerDeviceFailure,
    /// 0x05: запрос принят, выполнение долгое — ответ позже.
    Acknowledge,
    /// 0x06: устройство занято, повторить позже.
    ServerDeviceBusy,
    /// 0x07: программная функция не может быть выполнена.
    NegativeAcknowledge,
    /// 0x08: ошибка чётности памяти при чтении расширенного файла.
    MemoryParityError,
    /// 0x0A: шлюз не нашёл путь до целевого устройства.
    GatewayPathUnavailable,
    /// 0x0B: целевое устройство за шлюзом не ответило.
    GatewayTargetFailed,
    /// Нестандартный код — передаётся как есть.
    Other(u8),
}

impl Exception {
    /// Код на проводе.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::IllegalFunction => 0x01,
            Self::IllegalDataAddress => 0x02,
            Self::IllegalDataValue => 0x03,
            Self::ServerDeviceFailure => 0x04,
            Self::Acknowledge => 0x05,
            Self::ServerDeviceBusy => 0x06,
            Self::NegativeAcknowledge => 0x07,
            Self::MemoryParityError => 0x08,
            Self::GatewayPathUnavailable => 0x0A,
            Self::GatewayTargetFailed => 0x0B,
            Self::Other(code) => code,
        }
    }

    /// Исключение по коду с провода.
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            0x01 => Self::IllegalFunction,
            0x02 => Self::IllegalDataAddress,
            0x03 => Self::IllegalDataValue,
            0x04 => Self::ServerDeviceFailure,
            0x05 => Self::Acknowledge,
            0x06 => Self::ServerDeviceBusy,
            0x07 => Self::NegativeAcknowledge,
            0x08 => Self::MemoryParityError,
            0x0A => Self::GatewayPathUnavailable,
            0x0B => Self::GatewayTargetFailed,
            other => Self::Other(other),
        }
    }
}

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

/// Транспорт Modbus-master'а: кадр запроса — устройству, кадр ответа —
/// обратно.
///
/// Реализуют `adapters::modbus::{Rtu, Tcp, Udp}`: кадр RTU с CRC поверх
/// потока, MBAP поверх потока TCP, MBAP в датаграмме UDP. Логика master'а
/// (`domain::modbus::master`) работает с PDU — функция и данные — и про
/// транспорт не знает.
///
/// Транспорт отвечает за то, что лежит вне PDU: адрес устройства в ответе
/// совпадает с запрошенным, контрольная сумма (RTU) или номер транзакции
/// (TCP, UDP) сходятся. Всё, что внутри PDU, сверяет логика master'а.
#[must_use]
pub trait ModbusClient {
    /// Отказ транспорта.
    type Error;

    /// Отправляет PDU `request` устройству `unit` и ждёт ответ; кладёт PDU
    /// ответа в начало `response` и возвращает его длину.
    ///
    /// Таймаута у транспорта нет — ждать ответ сколько угодно; ограничить
    /// ожидание — дело вызывающего (`embassy_time::with_timeout`).
    ///
    /// # Errors
    ///
    /// Отказ транспорта, конец потока, неверная CRC, ответ не от `unit` —
    /// [`MasterError`]; исключение slave'а транспорт не разбирает, это PDU.
    ///
    /// # Panics
    ///
    /// Если `request` длиннее, а `response` короче [`MAX_PDU`], или `unit`
    /// вне адресов транспорта.
    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, MasterError<Self::Error>>;

    /// Отправляет PDU `request` широковещательно, без ответа.
    ///
    /// # Errors
    ///
    /// Отказ транспорта.
    ///
    /// # Panics
    ///
    /// Если `request` длиннее [`MAX_PDU`].
    async fn broadcast(&mut self, request: &[u8]) -> Result<(), MasterError<Self::Error>>;
}

/// Принятый slave'ом запрос: кому он и сколько байт PDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Received {
    /// Адрес устройства: 0 — широковещательный, 0xFF — «этому устройству,
    /// каким бы ни был его адрес» (так отдают любой запрос TCP и UDP: они
    /// адресуют устройство по IP).
    pub unit: u8,
    /// Длина PDU запроса в буфере `recv`, не меньше 1.
    pub len: usize,
}

/// Транспорт Modbus-slave'а: принимает кадры запросов, отправляет кадры
/// ответов.
///
/// Реализуют `adapters::modbus::{Rtu, Tcp, Udp}` (см. [`ModbusClient`]).
/// Битые кадры (CRC, MBAP с чужим протоколом, длина не сходится) транспорт
/// отбрасывает сам и ждёт следующий: до логики slave'а доходят только целые
/// PDU.
///
/// Ответ — на последний принятый запрос: транспорт помнит, что нужно, чтобы
/// его адресовать (номер транзакции MBAP, отправитель датаграммы). Slave
/// обслуживает запросы по одному, так что этого достаточно.
#[must_use]
pub trait ModbusServer {
    /// Отказ транспорта.
    type Error;

    /// Ждёт следующий целый запрос; кладёт его PDU в начало `buf`.
    ///
    /// # Errors
    ///
    /// Отказ транспорта, конец потока, потерянная граница кадра —
    /// [`ServeError`].
    ///
    /// # Panics
    ///
    /// Если `buf` короче [`MAX_PDU`].
    async fn recv(&mut self, buf: &mut [u8]) -> Result<Received, ServeError<Self::Error>>;

    /// Отправляет PDU ответа на последний принятый запрос.
    ///
    /// # Errors
    ///
    /// Отказ транспорта — [`ServeError`].
    ///
    /// # Panics
    ///
    /// Если `response` длиннее [`MAX_PDU`].
    async fn reply(&mut self, response: &[u8]) -> Result<(), ServeError<Self::Error>>;
}

/// Почему slave перестал обслуживать линию (`domain::modbus::slave::serve`).
///
/// Только отказы транспорта: битый кадр, чужой адрес, недопустимый запрос
/// slave переживает сам — пропускает или отвечает исключением.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum ServeError<E> {
    /// Отказ транспорта. `{0:?}`, а не `{0}`: транспорт не обещает `Display`.
    #[error("отказ транспорта: {0:?}")]
    Io(E),
    /// Поток закончился: соединение закрыто.
    #[error("поток закончился")]
    Eof,
    /// Кадр не по протоколу там, где границу следующего не найти (длина в
    /// заголовке MBAP вне допустимой): поток не выровнять, соединение стоит
    /// переоткрыть.
    #[error("граница кадра в потоке потеряна")]
    Framing,
}

/// Что может пойти не так в запросе master'а (`domain::modbus::master`).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum MasterError<E> {
    /// Отказ транспорта. `{0:?}`, а не `{0}`: транспорт не обещает `Display`.
    #[error("отказ транспорта: {0:?}")]
    Io(E),
    /// Поток закончился посреди ответа: соединение закрыто.
    #[error("поток закончился посреди ответа")]
    Eof,
    /// Контрольная сумма ответа не сошлась: помеха на линии (RTU).
    #[error("CRC ответа не сошлась")]
    Crc,
    /// Slave ответил исключением.
    #[error("slave ответил исключением {0:?}")]
    Exception(Exception),
    /// Целый кадр, но не ответ на этот запрос: чужой адрес, другая функция,
    /// не то количество, неразборное содержимое, кадр не по протоколу.
    #[error("ответ не соответствует запросу")]
    Unexpected,
}
