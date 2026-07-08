//! Иллюстративный `NonEmpty`-обёртка над `heapless::Vec`: непустота батча —
//! инвариант типа, а не проверка `Option`/`if is_empty()` в каждом месте
//! использования. Дополняет `heapless`, уже используемый в domain, а не
//! конкурирует с ним.
//!
//! `cargo run -p domain --example mitsein_nonempty_buffer`

use mitsein::heapless::vec1::Vec1;

fn main() {
    let mut batch: Vec1<u8, 8> = Vec1::from_array([1, 2, 3, 4]);
    batch.push(5).ok();

    // `first()` не требует `Option` — непустота гарантирована типом.
    assert_eq!(&1, batch.first());

    let sum: u32 = batch.iter1().into_iter().map(|byte| *byte as u32).sum();
    assert_eq!(sum, 1 + 2 + 3 + 4 + 5);
}
