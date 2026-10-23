//! Сырой образ из ELF и символы, которые из него читаются.

use std::{fs, path::Path};

use anyhow::Context;

use crate::memory::Region;

/// Имя символа, в котором прошивка хранит собственную версию.
///
/// Определён в `crates-cross/app/src/version.rs` под `#[unsafe(no_mangle)]` ровно
/// ради этого чтения.
const FW_VERSION_SYMBOL: &str = "FW_VERSION_IN_IMAGE";

/// Имя символа с открытым ключом, которым bootloader проверяет подпись.
/// Определён в `crates-cross/boot/src/signature.rs` так же, как
/// [`FW_VERSION_SYMBOL`] в приложении.
pub const PUBLIC_KEY_SYMBOL: &str = "OTA_PUBLIC_KEY_IN_IMAGE";

/// Байты, которыми заполняются пропуски между сегментами: стёртое состояние
/// NOR-флеша. Значение существенно — содержимое пропусков уезжает в раздел
/// `DFU` вместе с остальным образом, и разойдись оно со стёртым флешем, хеш
/// раздела на устройстве не совпал бы с посчитанным на хосте.
const ERASED_FLASH: u8 = 0xFF;

/// Верхняя граница размаха сырого образа. Флеш серии STM32 — до единиц мегабайт;
/// 64 MiB с запасом больше любого реального образа и заведомо меньше разрыва
/// между флешем и RAM.
const MAX_IMAGE_SPAN: u64 = 64 * 1024 * 1024;

/// Достаёт версию прошивки из собранного образа.
///
/// Через таблицу символов ELF, а не разбором `crates-cross/Cargo.toml`: в
/// манифесте лежит то, что попросили, а в образе — то, что получилось.
/// Разойтись они могут тихо (своя `version` у пакета вместо наследования от
/// воркспейса), и тогда устройство сравнивало бы номер, которого в прошивке
/// нет.
pub fn image_version(elf: &Path, base: u64, image: &[u8]) -> Result<u32, anyhow::Error> {
    let raw = image_symbol(elf, base, image, FW_VERSION_SYMBOL, 4)?;
    Ok(u32::from_le_bytes(
        raw.try_into().expect("ровно четыре байта"),
    ))
}

/// Байты статика `name` длиной `len` — такими, какими они лежат в образе.
pub fn image_symbol<'a>(
    elf: &Path,
    base: u64,
    image: &'a [u8],
    name: &str,
    len: usize,
) -> Result<&'a [u8], anyhow::Error> {
    use object::elf::PT_LOAD;
    use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};
    use object::read::{Object, ObjectSymbol};

    let bytes = fs::read(elf).with_context(|| format!("прочитать {}", elf.display()))?;
    let file = ElfFile32::<object::Endianness>::parse(&*bytes)
        .with_context(|| format!("разобрать {}", elf.display()))?;

    let symbol = file
        .symbols()
        .find(|symbol| symbol.name() == Ok(name))
        .with_context(|| {
            format!(
                "в {} нет символа {name} (версию определяет crates-cross/app/src/version.rs, ключ — \
                 crates-cross/boot/src/signature.rs): либо прошивка собрана без него, либо статик \
                 выбросили при линковке (тогда верните ему #[used] и #[unsafe(no_mangle)])",
                elf.display(),
            )
        })?;
    let address = symbol.address();

    // Символ адресуется виртуальным адресом, а образ собран по физическим (см.
    // `raw_image`). У `.rodata`, где живёт версия, они совпадают, но
    // рассчитывать на это нельзя: сегмент ищется явно, и адрес переводится
    // через его собственную пару vaddr/paddr. Ошибись мы тут молча — в хвост
    // образа уехали бы четыре чужих байта, и устройство сравнивало бы не
    // версию.
    let endian = file.endian();
    let segment = file
        .elf_header()
        .program_headers(endian, &*bytes)
        .ok()
        .and_then(|headers| {
            headers.iter().find(|header| {
                let vaddr = u64::from(header.p_vaddr(endian));
                // `p_filesz`, как и в `raw_image`: символ в хвосте `.bss` лежит вне образа,
                // и читать его из файла нельзя.
                let size = u64::from(header.p_filesz(endian));
                header.p_type(endian) == PT_LOAD
                    && address >= vaddr
                    && address - vaddr < size.max(1)
            })
        })
        .with_context(|| {
            format!("{name} по адресу {address:#x} не попал ни в один сегмент PT_LOAD")
        })?;

    let physical =
        u64::from(segment.p_paddr(endian)) + (address - u64::from(segment.p_vaddr(endian)));
    let offset = physical
        .checked_sub(base)
        .and_then(|offset| usize::try_from(offset).ok())
        .with_context(|| {
            format!("{name} лежит по адресу {physical:#x}, а образ начинается с {base:#x}")
        })?;
    image.get(offset..offset + len).with_context(|| {
        format!(
            "{name} по смещению {offset:#x} не помещается в образ длиной {}",
            image.len(),
        )
    })
}

/// Разбирает упакованную версию обратно — только чтобы напечатать её человеку.
///
/// Формула та же, что в `domain::ota::version::pack`, и живёт она там; здесь
/// повторяется, потому что `xtask` — член корневого воркспейса и зависимости
/// на `domain` у него нет (а заводить её ради трёх сдвигов дороже, чем эти
/// три сдвига).
pub fn unpack_version(version: u32) -> (u8, u8, u16) {
    (
        (version >> 24) as u8,
        ((version >> 16) & 0xFF) as u8,
        (version & 0xFFFF) as u16,
    )
}

/// Начало образа обязано совпасть с началом раздела приложения.
///
/// Отдельная функция, потому что сверять это место должны ДВА места, а не
/// одно: `build`, где следствие — испорченный `.bin` с честной подписью, и
/// прошивка перед тестами на плате ([`crate::flash`]), где следствие —
/// испорченная плата. Прошивка необратима, и
/// линкер расхождение не ловит (см. [`crate::size::report_image`]), так что проверка перед
/// записью в железо — не «перестраховка ради красоты», а последний момент,
/// когда ещё можно сказать «не надо».
pub fn ensure_image_base(base: u64, name: &str, partition: &Region) -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        base == partition.origin,
        "образ начинается с {base:#x}, а раздел {name} — с {:#x}. Раскладка и линковка разошлись: \
         подписывать и заливать такой образ нельзя, каждый его байт сместится относительно того, \
         что ждёт устройство. Проверьте memory.x и rustflags в crates-cross/.cargo/config.toml \
         (в частности `--nmagic`)",
        partition.origin,
    );
    Ok(())
}

/// Укладывает сегменты в непрерывный образ, начиная с самого младшего адреса.
///
/// Отдельная функция от чтения ELF, потому что здесь единственное, чему есть
/// чем сломаться: смещения, дыры, наложения. Читать ELF умеет `object`, а вот
/// проверить укладку без такой функции было бы нечем — для этого понадобился
/// бы настоящий ELF-файл в тестах.
fn image_from_segments(segments: &[(u64, Vec<u8>)]) -> Result<(u64, Vec<u8>), anyhow::Error> {
    let Some(base) = segments.iter().map(|(address, _)| *address).min() else {
        anyhow::bail!("в ELF нет ни одного загружаемого сегмента");
    };
    let end = segments
        .iter()
        .map(|(address, bytes)| address + bytes.len() as u64)
        .max()
        .unwrap_or(base);

    // Потолок на размах образа: разрыв между сегментами (например, `vaddr` вместо
    // `p_paddr` для `.data`) иначе превратился бы в аллокацию на сотни мегабайт.
    anyhow::ensure!(
        end - base <= MAX_IMAGE_SPAN,
        "образ занимает {} байт от {base:#x} до {end:#x} — больше {MAX_IMAGE_SPAN}: сегменты \
         ELF разбросаны по адресному пространству, скорее всего, линкер положил один из них \
         не во флеш",
        end - base,
    );
    let mut image = vec![
        ERASED_FLASH;
        usize::try_from(end - base).expect("образ не должен превышать usize::MAX")
    ];
    let mut filled = vec![false; image.len()];
    for (address, bytes) in segments {
        let offset = usize::try_from(address - base).expect("адрес не должен превышать usize::MAX");
        for (index, byte) in bytes.iter().enumerate() {
            anyhow::ensure!(
                !filled[offset + index],
                "сегменты ELF накладываются по адресу {:#x}",
                base + (offset + index) as u64,
            );
            image[offset + index] = *byte;
            filled[offset + index] = true;
        }
    }
    Ok((base, image))
}

/// Сырой образ: то, что уезжает во флеш, без секций и символов ELF.
///
/// Разбор идёт через `ElfFile32` и заголовки программы, а не через общий
/// `object::File`: тому доступен только виртуальный адрес сегмента
/// (`ObjectSegment::address`), а нужен физический. Для `.data` они разные —
/// секция исполняется из RAM, но лежит во флеше, откуда её копирует
/// `cortex-m-rt` при старте. Возьми мы виртуальный, в образе появилась бы
/// дыра в сотни мегабайт между флешем и RAM.
pub fn raw_image(elf: &Path) -> Result<(u64, Vec<u8>), anyhow::Error> {
    use object::elf::PT_LOAD;
    use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};

    let bytes = fs::read(elf).with_context(|| format!("прочитать {}", elf.display()))?;
    let file = ElfFile32::<object::Endianness>::parse(&*bytes)
        .with_context(|| format!("разобрать {}", elf.display()))?;
    let endian = file.endian();

    let mut segments = Vec::new();
    for header in file.elf_header().program_headers(endian, &*bytes)? {
        if header.p_type(endian) != PT_LOAD {
            continue;
        }
        // `p_filesz`, а не `p_memsz`: разница между ними — `.bss`, которого в
        // файле нет и во флеш писать нечего (его обнуляет cortex-m-rt).
        let data = header
            .data(endian, &*bytes)
            .map_err(|()| anyhow::anyhow!("сегмент ELF выходит за границы файла"))?;
        if data.is_empty() {
            continue;
        }
        segments.push((u64::from(header.p_paddr(endian)), data.to_vec()));
    }
    image_from_segments(&segments)
}

#[cfg(test)]
mod tests {
    #[test]
    fn packs_segments_and_fills_gaps_with_erased_flash() {
        // Два сегмента с дырой в четыре байта между ними.
        let segments = vec![(0x0800_0000, vec![1, 2]), (0x0800_0006, vec![3])];

        let (base, image) = super::image_from_segments(&segments).expect("сегменты укладываются");

        assert_eq!(base, 0x0800_0000);
        assert_eq!(image, vec![1, 2, 0xFF, 0xFF, 0xFF, 0xFF, 3]);
    }

    #[test]
    fn refuses_overlapping_segments() {
        // Наложение означает, что ELF описывает два разных содержимого для
        // одного адреса: молча выбрать одно из них нельзя.
        let segments = vec![(0x0800_0000, vec![1, 2, 3]), (0x0800_0002, vec![4])];

        super::image_from_segments(&segments).expect_err("наложение должно отвергаться");
    }

    /// Формула та же, что в `domain::ota::version::pack`, но зависимости на
    /// `domain` у xtask нет — значит разъехаться они могут только молча.
    ///
    /// Ловит это ПАРА тестов: здесь и `packs_into_the_agreed_bit_layout` в
    /// `domain::ota::version`, где стоят те же три литерала. Поодиночке ни один
    /// из них расхождения не увидит — каждый проверяет свою сторону.
    #[test]
    fn unpacks_a_version_the_way_domain_packs_it() {
        // 1.2.3, 0.1.0 (версия свежего проекта) и максимум каждого поля.
        assert_eq!(super::unpack_version(0x0102_0003), (1, 2, 3));
        assert_eq!(super::unpack_version(0x0001_0000), (0, 1, 0));
        assert_eq!(super::unpack_version(0xFFFF_FFFF), (255, 255, 65535));
    }
}
