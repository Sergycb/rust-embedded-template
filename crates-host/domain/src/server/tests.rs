//! Тесты узла протокола: ответы на кадры и отказы канала.

use ports::{Handler, LinkError};

use super::fakes::{Link, serve};

/// Отвечает кадром наоборот; на пустой кадр не отвечает.
struct Reverse;

impl Handler for Reverse {
    fn handle(&mut self, frame: &mut [u8], len: usize) -> impl Future<Output = Option<usize>> {
        frame[..len].reverse();
        std::future::ready((len > 0).then_some(len))
    }
}

#[test]
fn answers_each_frame_and_stays_silent_when_the_handler_is() {
    let mut link = Link::with([Ok(vec![1, 2, 3]), Ok(vec![]), Ok(vec![4, 5])], 8);
    serve(&mut link, &mut Reverse);
    assert_eq!(link.sent, [vec![3, 2, 1], vec![5, 4]]);
}

/// Потерянный кадр и закрытый канал узел переживает: следующий кадр —
/// ответ.
#[test]
fn lost_frames_and_a_closed_link_do_not_stop_the_node() {
    let mut link = Link::with(
        [
            Err(LinkError::Lost("помеха")),
            Err(LinkError::Down("кабель вынут")),
            Ok(vec![1, 2]),
        ],
        8,
    );
    serve(&mut link, &mut Reverse);
    assert_eq!(link.sent, [vec![2, 1]]);
}

/// Ответ, не ушедший в канал, — отказом или потерей, — узел не
/// останавливает: он ждёт следующий кадр и отвечает на него.
#[test]
fn failed_sends_do_not_stop_the_node() {
    for send in [
        Err(LinkError::Down("обрыв")),
        Err(LinkError::Lost("не влез")),
    ] {
        let mut link = Link::with([Ok(vec![1]), Ok(vec![2])], 8);
        link.send = send;
        serve(&mut link, &mut Reverse);
        assert_eq!(link.sent.len(), 2, "оба кадра обработаны");
    }
}
