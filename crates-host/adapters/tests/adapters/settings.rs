//! Хранилище настроек поверх `sequential-storage`.

use ports::SettingsStorage;

use adapters::settings::Settings;

use crate::mem_flash::MemFlash;

/// Две страницы по 256 байт, слово 8 — минимум, который берёт
/// `sequential-storage`.
type Flash = MemFlash<512, 256, 8>;

#[tokio::test]
async fn stores_and_reads_back() {
    let mut settings = Settings::new(Flash::erased(), 0..512);
    let mut scratch = [0u8; 32];

    settings.write(7, b"template").await.expect("запись");
    let back = settings.read(7, &mut scratch).await.expect("чтение");

    assert_eq!(back, Some(&b"template"[..]));
}

#[tokio::test]
async fn a_missing_key_reads_as_none() {
    let mut settings = Settings::new(Flash::erased(), 0..512);
    let mut scratch = [0u8; 32];

    assert_eq!(settings.read(1, &mut scratch).await.expect("чтение"), None);
}

#[tokio::test]
async fn a_later_write_shadows_the_earlier_one() {
    let mut settings = Settings::new(Flash::erased(), 0..512);
    let mut scratch = [0u8; 32];

    settings.write(3, b"first").await.expect("первая");
    settings.write(3, b"second").await.expect("вторая");

    assert_eq!(
        settings.read(3, &mut scratch).await.expect("чтение"),
        Some(&b"second"[..])
    );
}

#[test]
#[should_panic(expected = "не годится под sequential-storage")]
fn refuses_a_partition_smaller_than_two_pages() {
    let _ = Settings::new(Flash::erased(), 0..256);
}
