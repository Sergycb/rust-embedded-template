//! Адаптер OTA поверх `embassy-boot`: стирание под образ, запись, состояние
//! разделов, обёртка с подписью.

#[cfg(feature = "signed")]
use adapters::ota::Signed;
use adapters::ota::{Error, Updater};

use crate::mem_flash::MemFlash;
use embedded_storage::nor_flash::NorFlashErrorKind;
use ports::FirmwareUpdate;

/// Четыре страницы по 256 байт, слово 8; ACTIVE (= capacity) — три
/// страницы, как требует запас на обмен.
type Dfu = MemFlash<1024, 256, 8>;
/// Раздел состояния: страница на магию плюс журнал прогресса.
type State = MemFlash<256, 256, 8>;

fn updater() -> Updater<Dfu, State> {
    Updater::new(Dfu::erased(), State::erased(), 768)
}

/// Весь раздел `DFU` — через порт, как его видит приложение: поле с флешем
/// у `Updater` приватное.
fn read_dfu(u: &mut Updater<Dfu, State>) -> [u8; 1024] {
    let mut dfu = [0; 1024];
    u.read(0, &mut dfu).expect("чтение раздела");
    dfu
}

fn with_state(magic: u8) -> Updater<Dfu, State> {
    let mut state = State::erased();
    state.mem[..8].fill(magic);
    Updater::new(Dfu::erased(), state, 768)
}

#[test]
fn prepare_erases_only_the_pages_the_image_needs() {
    let mut u = Updater::new(Dfu::filled(0x00), State::erased(), 768);

    u.prepare(257).expect("стереть под образ");

    let dfu = read_dfu(&mut u);
    assert!(dfu[..512].iter().all(|b| *b == 0xFF), "две страницы стёрты");
    assert!(dfu[512..].iter().all(|b| *b == 0x00), "третья не тронута");
}

#[test]
fn prepare_refuses_zero_and_oversized_lengths_before_erasing() {
    for len in [0, 769] {
        let mut u = Updater::new(Dfu::filled(0x00), State::erased(), 768);

        let refused = u.prepare(len);

        assert!(
            matches!(refused, Err(Error::Flash(NorFlashErrorKind::OutOfBounds))),
            "{len}"
        );
        assert!(
            read_dfu(&mut u).iter().all(|b| *b == 0x00),
            "раздел не должен быть стёрт"
        );
    }
}

#[test]
fn write_and_prepare_refuse_while_the_partition_holds_a_rollback_image() {
    // 0xF0 — SWAP_MAGIC, 0xC0 — REVERT_MAGIC (`embassy-boot`, lib.rs).
    for magic in [0xF0, 0xC0] {
        let mut u = with_state(magic);

        assert!(u.is_busy().expect("состояние"), "{magic:#x}");
        assert!(matches!(u.prepare(8), Err(Error::BadState)));
        assert!(matches!(u.write(0, &[0; 8]), Err(Error::BadState)));
        assert!(
            u.read(0, &mut [0; 8]).is_ok(),
            "чтение занятого раздела разрешено"
        );
    }
}

#[test]
fn a_booted_state_is_not_busy() {
    // 0xD0 — BOOT_MAGIC; стёртый раздел читается как Boot тоже.
    assert!(!with_state(0xD0).is_busy().expect("состояние"));
    assert!(!updater().is_busy().expect("состояние"));
}

#[test]
fn writes_and_reads_back_after_prepare() {
    let mut u = updater();
    u.prepare(16).expect("подготовка");

    u.write(0, &[0xA5; 8]).expect("первая запись");
    u.write(8, &[0x5A; 8])
        .expect("вторая запись в тот же сектор");
    let mut back = [0; 16];
    u.read(0, &mut back).expect("чтение");

    assert_eq!(&back[..8], &[0xA5; 8]);
    assert_eq!(&back[8..], &[0x5A; 8]);
}

#[test]
fn mark_updated_makes_the_partition_busy_and_mark_booted_frees_it() {
    let mut u = updater();

    u.mark_updated().expect("пометить обмен");
    assert!(u.is_busy().expect("состояние"));

    u.mark_booted().expect("подтвердить");
    assert!(!u.is_busy().expect("состояние"));
}

/// Порт объявляет `mark_updated` безусловно, но у `Signed` он не должен
/// давать обменять разделы мимо проверки подписи — единственный путь
/// туда лежит через `domain::ota::update::apply_signed`.
#[cfg(feature = "signed")]
#[test]
fn signed_mark_updated_always_refuses() {
    let mut signed = Signed::new(updater(), 1, [7; 32]);

    assert!(matches!(signed.mark_updated(), Err(Error::BadState)));
    assert!(
        !signed.is_busy().expect("состояние"),
        "состояние не тронуто"
    );
}

/// Подпись, которая не может сойтись (ключ и подпись — константы), доходит
/// до порта своим вариантом, а не как отказ флеша.
#[cfg(feature = "signed")]
#[test]
fn signed_reports_a_bad_signature_as_such() {
    use ports::{SignedFirmwareUpdate, VerifyError};
    let mut signed = Signed::new(updater(), 1, [7; 32]);
    signed.prepare(64).expect("стереть под образ");
    signed.write(0, &[0xA5; 64]).expect("записать образ");

    assert!(matches!(
        signed.verify_and_mark_updated(&[9; 64], 64),
        Err(VerifyError::BadSignature)
    ));
    assert!(
        !signed.is_busy().expect("состояние"),
        "негодная подпись не должна помечать обмен"
    );
}

#[test]
fn reports_the_flash_geometry() {
    let mut u = updater();
    assert_eq!(u.write_granularity(), 8);
    assert_eq!(u.erase_size(), 256);
    assert_eq!(u.capacity().expect("вместимость"), 768);
}
