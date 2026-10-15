//! Транспорты Modbus: RTU поверх потока байтов, TCP поверх потока, UDP
//! поверх датаграмм. Каждый реализует оба порта — [`ports::ModbusClient`]
//! для master'а и [`ports::ModbusServer`] для slave'а, — и логика
//! `domain::modbus` работает с PDU, не зная, какой транспорт под ней.
//!
//! Чем транспорты отличаются, и почему каждый устроен так:
//!
//! * [`Rtu`](crate::modbus::Rtu) — кадр «адрес, PDU, CRC» поверх
//!   `embedded_io_async::{Read, Write}`. Пауза между кадрами в потоке не
//!   видна, поэтому граница кадра выводится из содержимого: длина — из
//!   кода функции и счётчика байт, CRC подтверждает, что кадр найден верно;
//!   после битого кадра slave сдвигается на байт, пока CRC не сойдётся
//!   снова.
//! * [`Tcp`](crate::modbus::Tcp) — заголовок MBAP (номер транзакции,
//!   протокол 0, длина, адрес) и PDU поверх потока. CRC нет — целостность
//!   обеспечивает TCP, границу — длина в заголовке. Длина вне допустимой —
//!   граница потеряна, и поток не выровнять: slave кончается
//!   [`ServeError::Framing`](ports::ServeError::Framing), master —
//!   [`MasterError::Unexpected`](ports::MasterError::Unexpected).
//! * [`Udp`](crate::modbus::Udp) — тот же MBAP, по запросу или ответу в
//!   датаграмме (`ports::FrameLink`): границу даёт датаграмма, и битая
//!   просто пропускается.
//!
//! Номер транзакции master'а растёт с каждым запросом, ответ с чужим
//! номером — опоздавший ответ на брошенный по таймауту запрос — пропускается.
//! В RTU номера нет, и там то же делает слив входного буфера перед запросом.
//!
//! TCP и UDP адресуют устройство по IP, и адрес в MBAP нужен только шлюзу
//! на шину RS-485. Поэтому slave получает любой запрос по TCP и UDP как
//! адресованный ему (0xFF), а ответ несёт адрес из запроса. Master
//! по-прежнему шлёт адресу 0 без ожидания ответа
//! ([`ports::ModbusClient::broadcast`]): ответ, если он придёт, будет
//! пропущен по номеру транзакции; к самому устройству за TCP master
//! обращается адресом 0xFF.

/// Заголовок MBAP — общий для TCP и UDP.
mod mbap;

/// Modbus RTU: кадр с CRC поверх потока байтов.
mod rtu;

/// Modbus TCP: MBAP поверх потока байтов.
mod tcp;

/// Modbus UDP: MBAP в датаграмме.
mod udp;

#[cfg(test)]
mod fakes;

pub use rtu::Rtu;
pub use tcp::Tcp;
pub use udp::Udp;

use embedded_io_async::{Read, ReadExactError};
use ports::{MasterError, ServeError};

/// Дочитывает `rx` из `io` до `want` байт. Каждый прочитанный кусок сразу
/// учтён в `have`, поэтому брошенное чтение ничего не теряет: следующий
/// вызов продолжит с того же места.
async fn fill<T: Read>(
    io: &mut T,
    rx: &mut [u8],
    have: &mut usize,
    want: usize,
) -> Result<(), ReadExactError<T::Error>> {
    while *have < want {
        match io.read(&mut rx[*have..want]).await {
            Ok(0) => return Err(ReadExactError::UnexpectedEof),
            Ok(n) => *have += n,
            Err(e) => return Err(ReadExactError::Other(e)),
        }
    }
    Ok(())
}

fn serve_error<E>(e: ReadExactError<E>) -> ServeError<E> {
    match e {
        ReadExactError::UnexpectedEof => ServeError::Eof,
        ReadExactError::Other(e) => ServeError::Io(e),
    }
}

fn master_error<E>(e: ReadExactError<E>) -> MasterError<E> {
    match e {
        ReadExactError::UnexpectedEof => MasterError::Eof,
        ReadExactError::Other(e) => MasterError::Io(e),
    }
}
