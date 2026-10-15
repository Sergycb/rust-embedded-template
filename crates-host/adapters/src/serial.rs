//! Поток байтов, которого ещё нет на плате, — заглушка в слоте UART.

use core::convert::Infallible;

use embedded_io_async::{ErrorType, Read, ReadReady, Write};

/// UART, который ещё не подключили, — место под драйвер платы.
///
/// Шаблон не выбирает выводы за проект (`docs/architecture.md`): узел
/// протокола получает поток байтов, а какой это USART и на каких ножках,
/// знает только плата. До тех пор в слоте стоит эта заглушка — как
/// [`flash::Unconnected`](crate::flash::Unconnected) в слоте внешней флеши и
/// `bsp::ota::Link` в слоте канала OTA. Проект с ней собирается и
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
