//! Фейк кадрового канала и прогон узла над ним — для тестов узла и
//! обработчиков, которые гоняют через него.

use std::collections::VecDeque;

use core::pin::pin;
use core::task::{Context, Waker};
use ports::{FrameLink, LinkError};

use super::{Handler, ServerCtx, task};

/// Канал: отдаёт заранее заготовленные исходы `recv`, а когда они
/// кончаются, ждёт следующего кадра вечно — как настоящий канал.
/// Отправленные кадры складывает в `sent`.
pub(crate) struct Link {
    pub(crate) incoming: VecDeque<Result<Vec<u8>, LinkError<&'static str>>>,
    pub(crate) sent: Vec<Vec<u8>>,
    pub(crate) send: Result<(), LinkError<&'static str>>,
    pub(crate) buffer: Vec<u8>,
}

impl Link {
    /// Канал с кадрами `incoming`, исправной отправкой и буфером на
    /// `capacity` байт.
    pub(crate) fn with(
        incoming: impl IntoIterator<Item = Result<Vec<u8>, LinkError<&'static str>>>,
        capacity: usize,
    ) -> Self {
        Self {
            incoming: incoming.into_iter().collect(),
            sent: Vec::new(),
            send: Ok(()),
            buffer: vec![0; capacity],
        }
    }
}

impl FrameLink for Link {
    type Error = &'static str;

    fn recv(&mut self) -> impl Future<Output = Result<usize, LinkError<Self::Error>>> {
        let buf = &mut self.buffer;
        let next = self.incoming.pop_front().map(|next| {
            next.map(|frame| {
                buf[..frame.len()].copy_from_slice(&frame);
                frame.len()
            })
        });
        async move {
            match next {
                Some(next) => next,
                None => std::future::pending().await,
            }
        }
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    fn send(&mut self, len: usize) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
        self.sent.push(self.buffer[..len].to_vec());
        std::future::ready(self.send)
    }
}

/// Гоняет узел над `link` и `handler` по всем заготовленным кадрам, пока
/// он не станет ждать следующий.
pub(crate) fn serve<H: Handler>(link: &mut Link, handler: &mut H) {
    let ctx = ServerCtx {
        link,
        handler,
        index: 0,
    };
    let mut task = pin!(task(ctx));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт кадр");
}
