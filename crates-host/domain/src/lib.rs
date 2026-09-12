#![cfg_attr(not(test), no_std)]

// cfg(kani) объявлен в [workspace.lints.rust] корневого Cargo.toml.
#[cfg(kani)]
mod kani_proofs;

/// Крутит future фейков в host-тестах: ни один порт-фейк не ждёт по-настоящему,
/// поэтому исполнитель здесь не нужен — достаточно одного `poll`.
#[cfg(test)]
pub(crate) mod test_support {
    use core::future::Future;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    pub fn block_on<T>(future: impl Future<Output = T>) -> T {
        let mut future = pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("фейки портов не ждут: future не должна возвращать Pending"),
        }
    }
}

/// Версия прошивки и правило приёма обновления — см. модуль.
///
/// Первый настоящий модуль домена в шаблоне и заодно образец границы: `bsp`
/// достаёт четыре байта из флеша, `domain` решает, что с ними делать.
pub mod firmware;

/// Приём образа по кускам: сверка длины, стирание один раз, буферизация до
/// слова флеша. Функция над портами `ImageSource` и `FirmwareUpdate` — см.
/// модуль.
pub mod download;

/// Задача узла `APP`: то, что граф из `crates-cross/app` спавнит, а делает —
/// домен. Каркас под работу вашего устройства, см. модуль.
pub mod app;
