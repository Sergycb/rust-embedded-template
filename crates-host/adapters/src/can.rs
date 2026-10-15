//! Шина CAN, которой ещё нет на плате, — заглушка в слоте CAN.

use core::convert::Infallible;
use core::marker::PhantomData;

use ports::{CanBus, LinkError};

/// CAN, который ещё не подключили, — место под контроллер платы.
///
/// Как `serial::Unconnected` в слоте UART:
/// шаблон не выбирает выводы, а узел CANopen получает шину через
/// `Board::can`. Проект с заглушкой собирается и запускается: приём ждёт
/// вечно, и узел спит, не занимая исполнитель, а отправка молча
/// выбрасывает кадр. Кадр `F` — тот же тип, что у настоящего драйвера
/// (`embassy_stm32::can::Frame`), поэтому заменить заглушку — значит в
/// `bsp::can` поменять один псевдоним.
#[derive(Debug)]
pub struct Unconnected<F>(PhantomData<F>);

impl<F> Default for Unconnected<F> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<F: embedded_can::Frame> CanBus for Unconnected<F> {
    type Frame = F;
    type Error = Infallible;

    async fn recv(&mut self) -> Result<F, LinkError<Infallible>> {
        core::future::pending().await
    }

    fn send(&mut self, _: &F) -> impl Future<Output = Result<(), LinkError<Infallible>>> {
        core::future::ready(Ok(()))
    }
}
