//! Поток байтов, которого ещё нет на плате, — заглушка в слоте UART.

use core::convert::Infallible;

use embedded_io_async::{ErrorType, Read, ReadReady, Write};

/// UART, который ещё не подключили, — место под драйвер платы.
///
/// Шаблон не выбирает выводы за проект (`docs/architecture.md`): узел
/// протокола получает поток байтов, а какой это USART и на каких ножках,
/// знает только плата. До тех пор в слоте стоит эта заглушка — как
/// [`flash::Unconnected`](crate::flash::Unconnected) в слоте внешней флеши и
/// `app::Link` в слоте канала OTA. Проект с ней собирается и
/// запускается: чтение ждёт вечно, и узел протокола спит, не занимая
/// исполнитель, а запись молча выбрасывает байты. Заменить её — значит в
/// `bsp::serial` подставить `BufferedUart` своей платы.
#[derive(Debug, Default)]
pub struct Unconnected;

impl ErrorType for Unconnected {
    type Error = Infallible;
}

impl Read for Unconnected {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, Infallible> {
        core::future::pending().await
    }
}

impl ReadReady for Unconnected {
    fn read_ready(&mut self) -> Result<bool, Infallible> {
        Ok(false)
    }
}

impl Write for Unconnected {
    fn write(&mut self, buf: &[u8]) -> impl Future<Output = Result<usize, Infallible>> {
        core::future::ready(Ok(buf.len()))
    }

    fn flush(&mut self) -> impl Future<Output = Result<(), Infallible>> {
        core::future::ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Waker};

    use super::*;

    /// Заглушка не отдаёт ни байта и не кончается — чтение ждёт вечно, а
    /// запись и сброс проходят, выбрасывая байты.
    #[tokio::test]
    async fn waits_forever_and_swallows_writes() {
        let mut serial = Unconnected;
        let mut buf = [0; 4];
        let mut cx = Context::from_waker(Waker::noop());
        assert!(pin!(serial.read(&mut buf)).poll(&mut cx).is_pending());
        assert_eq!(serial.read_ready(), Ok(false));
        assert_eq!(serial.write(b"abc").await, Ok(3));
        assert_eq!(serial.flush().await, Ok(()));
    }
}
