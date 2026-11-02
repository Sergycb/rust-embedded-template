//! Словарь Modbus для портов: предел PDU, коды исключений и данные slave'а.
//!
//! Данные slave'а — трейт [`Registers`](crate::Registers) и представления
//! кадра (`registers`): его реализуют карта проекта и обёртки над ней в
//! `domain::modbus`, а зовёт обработчик `domain::modbus::Slave`.
//! Транспорт slave'а — канал [`FrameLink`](crate::FrameLink) с кадром «адрес
//! и PDU», его реализует `adapters_logic::modbus` (RTU, TCP, UDP).
// {%- if modbus_master == "true" %}
//! Транспорт master'а — [`ModbusClient`](crate::ModbusClient) — реализуют они
//! же; его ошибка — [`MasterError`](crate::modbus::MasterError)
//! (`domain::modbus::master`).
// {%- endif %}

/// Данные slave'а: трейт карты и представления битов и регистров кадра.
mod registers;

pub use registers::{Bits, BitsMut, Registers, Words, WordsMut};

/// Наибольший PDU Modbus — функция и данные: 253 байта. Предел пришёл из
/// кадра RTU (256 байт с адресом и CRC) и одинаков для всех транспортов.
/// Буферы PDU транспортов — не короче.
pub const MAX_PDU: usize = 253;

/// Код исключения Modbus — ответ slave'а вместо данных.
///
/// Карта slave'а ([`Registers`]) возвращает обычно первые
/// четыре; на проводе бывает любой, включая коды шлюза и нестандартные
/// ([`Exception::Other`]).
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

// {%- if modbus_master == "true" %}
/// Транспорт Modbus-master'а: кадр запроса — устройству, кадр ответа —
/// обратно.
///
/// Реализуют `adapters_logic::modbus::{Rtu, Tcp, Udp}`: кадр RTU с CRC поверх
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
// {%- endif %}
// {%- if modbus_master == "true" %}
/// Почему транспорт master'а не доставил ответ: то, что вне PDU. Исключение
/// slave'а и PDU не на тот запрос — ошибки логики master'а
/// (`domain::modbus::master::RequestError`), транспорт их не разбирает.
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
    /// Целый кадр, но не ответ на этот запрос: чужой адрес, длина вне
    /// допустимой, кадр не по протоколу. То, что внутри PDU, транспорт не
    /// сверяет — это делает логика master'а.
    #[error("ответ не соответствует запросу")]
    Unexpected,
}
// {%- endif %}

#[cfg(test)]
mod tests {
    use super::Exception;

    #[test]
    fn exception_code_round_trips_for_every_code() {
        for code in 0..=u8::MAX {
            assert_eq!(Exception::from_code(code).code(), code, "код {code:#04x}");
        }
        assert_eq!(
            Exception::from_code(0x0A),
            Exception::GatewayPathUnavailable
        );
        assert_eq!(Exception::from_code(0x09), Exception::Other(0x09));
    }
}
