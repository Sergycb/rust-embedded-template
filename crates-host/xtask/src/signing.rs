//! Ключи подписи OTA-образов и сама подпись.

use std::{fs, path::Path};

use anyhow::Context;

use crate::image::{PUBLIC_KEY_SYMBOL, image_symbol};

/// Файл с закрытым ключом подписи. Лежит в корне проекта и внесён в
/// `.gitignore`: закоммитить его — то же самое, что не подписывать образы
/// вовсе.
const SIGNING_KEY_FILE: &str = "ota-signing-key.bin";

/// Открытый ключ, которым устройство проверяет подпись. В отличие от закрытого
/// — коммитится: именно он попадает в прошивку (его читает
/// `crates-cross/app/build.rs`), и без него собранный образ не примет ни одно
/// устройство.
pub const PUBLIC_KEY_FILE: &str = "ota-public-key.bin";

/// Сверяет ключ, скомпилированный в bootloader, с ключом, которым образ сейчас
/// будет подписан.
///
/// Единственная проверка того, что проводка ключа целая: файл →
/// `boot/build.rs` → `OTA_PUBLIC_KEY_IN_IMAGE` → `screen_update`. Сломайся она
/// (нули вместо ключа, чужой файл) — bootloader отвергал бы каждое обновление,
/// и узнали бы об этом на уже прошитом устройстве.
pub fn ensure_image_key(
    elf: &Path,
    base: u64,
    image: &[u8],
    seed: &[u8; 32],
) -> Result<(), anyhow::Error> {
    let compiled = image_symbol(elf, base, image, PUBLIC_KEY_SYMBOL, 32)?;
    let signing = salty::Keypair::from(seed).public.to_bytes();
    anyhow::ensure!(
        compiled == signing,
        "в bootloader'е не тот открытый ключ, которым подписывается образ: устройство отвергнет \
         это обновление. Пересоберите bootloader (`cargo clean -p boot`) — ключ попадает в \
         него через crates-cross/boot/build.rs из {PUBLIC_KEY_FILE}"
    );
    Ok(())
}

/// Подписывает образ: SHA-512 от его байтов, подпись самого хеша — так же
/// считает bootloader (`domain::ota::boot::screen_update`).
pub fn sign_image(image_path: &Path, image: &[u8], seed: &[u8; 32]) -> Result<(), anyhow::Error> {
    let signature = signature(image, seed);

    let path = image_path.with_extension("bin.sig");
    fs::write(&path, signature).with_context(|| format!("записать {}", path.display()))?;

    println!("подпись: {} (64 байта)", path.display());
    println!(
        "устройству нужны обе величины: подпись и длина образа ({} байт) — их несёт первая \
         команда обновления (Start), применяет узел domain::ota через apply_signed",
        image.len(),
    );
    Ok(())
}

/// Подпись образа без записи в файл — для инертного образа host-target теста.
pub fn signature(image: &[u8], seed: &[u8; 32]) -> [u8; 64] {
    let digest = salty::Sha512::new().updated(image).finalize();
    salty::Keypair::from(seed).sign(&digest).to_bytes()
}

/// Приводит пару ключей в согласованное состояние и возвращает закрытый.
///
/// Четыре случая, и только один из них — отказ:
///
/// * нет обоих файлов — создаётся пара (первая сборка проекта с подписью);
/// * есть закрытый, нет открытого — открытый выводится из закрытого. Корень
///   доверия при этом не меняется, поэтому делается тихо;
/// * есть оба — сверяется, что открытый выведен из этого закрытого;
/// * есть открытый, нет закрытого — `Ok(None)`: собрать можно, подписать нечем.
///
/// Последний случай — это каждый CI-прогон и каждый свежий клон: закрытый ключ
/// лежит в `.gitignore`, открытый коммитится. Выпускать там новый ключ нельзя
/// (прошивка перестала бы приниматься уже прошитыми устройствами), но и падать
/// не за что: для КОМПИЛЯЦИИ закрытый ключ не нужен вовсе — в прошивку уезжает
/// открытый. Поэтому сборка идёт дальше, а подпись пропускается с
/// предупреждением.
pub fn ensure_signing_key(root: &Path) -> Result<Option<[u8; 32]>, anyhow::Error> {
    let private_path = root.join(SIGNING_KEY_FILE);
    let public_path = root.join(PUBLIC_KEY_FILE);

    if !private_path.exists() {
        if public_path.exists() {
            println!(
                "{PUBLIC_KEY_FILE} есть, а {SIGNING_KEY_FILE} нет — образ собран, но НЕ подписан."
            );
            println!(
                "Так выглядит свежий клон и любой CI: закрытый ключ в .gitignore и остаётся у \
                 того, кто выпускает обновления."
            );
            println!(
                "Новый ключ здесь не создаётся намеренно: устройства, прошитые прежним, \
                 обновлений с ним не примут. Нужен именно новый корень доверия — удалите \
                 {PUBLIC_KEY_FILE}."
            );
            return Ok(None);
        }

        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).context("не удалось получить случайные байты от ОС")?;
        write_private(&private_path, &seed)
            .with_context(|| format!("записать {}", private_path.display()))?;
        write_public(&public_path, &seed)?;

        println!("создана ключевая пара для подписи образов:");
        println!(
            "  {} — закрытый, в .gitignore. Потеряете его — выпускать",
            private_path.display()
        );
        println!("  обновления для уже прошитых устройств станет нечем.");
        println!(
            "  {} — открытый, попадает в прошивку. Закоммитьте его.",
            public_path.display()
        );
        return Ok(Some(seed));
    }

    let seed = read_seed(&private_path)?;

    if public_path.exists() {
        let stored = fs::read(&public_path)
            .with_context(|| format!("прочитать {}", public_path.display()))?;
        anyhow::ensure!(
            stored == salty::Keypair::from(&seed).public.to_bytes(),
            "{PUBLIC_KEY_FILE} не соответствует {SIGNING_KEY_FILE}: открытый ключ выведен из \
             другого закрытого. Устройство, прошитое этим открытым ключом, не примет образ, \
             подписанный имеющимся закрытым",
        );
    } else {
        // Открытый восстанавливаем из закрытого, и это не смена корня доверия
        // — ключ тот же, просто файл не дошёл (свежий клон, чистка каталога).
        write_public(&public_path, &seed)?;
        println!(
            "{} восстановлен из закрытого ключа — закоммитьте его",
            public_path.display()
        );
    }

    Ok(Some(seed))
}

/// Закрытый ключ — 32 байта seed, из которых `salty` разворачивает пару.
fn read_seed(path: &Path) -> Result<[u8; 32], anyhow::Error> {
    let bytes = fs::read(path).with_context(|| format!("прочитать {}", path.display()))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!(
            "{} должен быть ровно 32 байта, а в нём {}",
            path.display(),
            bytes.len()
        )
    })
}

fn write_public(path: &Path, seed: &[u8; 32]) -> Result<(), anyhow::Error> {
    let public = salty::Keypair::from(seed).public.to_bytes();
    fs::write(path, public).with_context(|| format!("записать {}", path.display()))
}

/// Пишет файл, закрытый для всех, кроме владельца.
///
/// Права выставляются при создании, а не после: между `write` и `chmod` файл
/// с приватным ключом успел бы полежать читаемым для всех. На Windows прав
/// POSIX нет — там файл создаётся обычным способом, и защита сводится к тому,
/// что он в `.gitignore` и лежит только у вас.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{PUBLIC_KEY_FILE, SIGNING_KEY_FILE, ensure_signing_key};
    use crate::memory::scratch::scratch;

    fn public_of(seed: &[u8; 32]) -> [u8; 32] {
        salty::Keypair::from(seed).public.to_bytes()
    }

    #[test]
    fn creates_a_pair_when_nothing_is_there() {
        let dir = scratch("empty");

        let seed = ensure_signing_key(dir.path())
            .expect("пара должна создаться")
            .expect("подписывать есть чем");

        let stored = fs::read(dir.path().join(SIGNING_KEY_FILE)).expect("закрытый ключ записан");
        let public = fs::read(dir.path().join(PUBLIC_KEY_FILE)).expect("открытый ключ записан");
        assert_eq!(stored, seed);
        assert_eq!(public, public_of(&seed));
    }

    #[test]
    fn restores_public_key_from_private() {
        let dir = scratch("no-public");
        let seed = [7u8; 32];
        fs::write(dir.path().join(SIGNING_KEY_FILE), seed).expect("положить закрытый ключ");

        let returned = ensure_signing_key(dir.path())
            .expect("открытый ключ должен восстановиться")
            .expect("подписывать есть чем");

        assert_eq!(returned, seed);
        let public = fs::read(dir.path().join(PUBLIC_KEY_FILE)).expect("открытый ключ записан");
        assert_eq!(public, public_of(&seed));
    }

    /// Ветка, по которой идёт каждая вторая и последующая сборка, — и до сих
    /// пор не проверенная ни одним тестом. Переверни сравнение ключей, и все
    /// остальные тесты остались бы зелёными, а любая нормальная сборка стала
    /// бы падать с «пара не соответствует».
    #[test]
    fn accepts_a_matching_pair_and_leaves_it_alone() {
        let dir = scratch("matching");
        let seed = [7u8; 32];
        fs::write(dir.path().join(SIGNING_KEY_FILE), seed).expect("положить закрытый ключ");
        fs::write(dir.path().join(PUBLIC_KEY_FILE), public_of(&seed))
            .expect("положить парный открытый");

        let returned = ensure_signing_key(dir.path())
            .expect("согласованная пара должна приниматься")
            .expect("подписывать есть чем");

        assert_eq!(returned, seed);
        let stored = fs::read(dir.path().join(SIGNING_KEY_FILE)).expect("закрытый ключ на месте");
        assert_eq!(stored, seed, "закрытый ключ трогать не надо");
    }

    /// Главный случай этого набора: так выглядит каждый CI-прогон и каждый
    /// свежий клон — открытый ключ в репозитории, закрытый в `.gitignore`.
    /// Собрать при этом можно (в прошивку уезжает открытый), а вот выпустить
    /// новый ключ нельзя: устройства, прошитые прежним, обновлений не примут.
    #[test]
    fn builds_without_signing_when_only_the_public_key_is_here() {
        let dir = scratch("no-private");
        fs::write(dir.path().join(PUBLIC_KEY_FILE), [1u8; 32]).expect("положить открытый ключ");

        let seed = ensure_signing_key(dir.path()).expect("сборке закрытый ключ не нужен");

        assert!(seed.is_none(), "подписывать здесь нечем");
        assert!(
            !dir.path().join(SIGNING_KEY_FILE).exists(),
            "закрытый ключ не должен появиться: это был бы новый корень доверия"
        );
    }

    #[test]
    fn refuses_a_mismatched_pair() {
        let dir = scratch("mismatch");
        fs::write(dir.path().join(SIGNING_KEY_FILE), [7u8; 32]).expect("положить закрытый ключ");
        fs::write(dir.path().join(PUBLIC_KEY_FILE), [1u8; 32]).expect("положить чужой открытый");

        ensure_signing_key(dir.path()).expect_err("несогласованная пара должна отвергаться");
    }
}
