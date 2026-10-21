//! Сколько образ занимает от раздела: отчёт, дельта к ветке по умолчанию, бюджет.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

use crate::image::ensure_image_base;
use crate::memory::{app_memory_regions, app_region, boot_memory_regions, region};

/// Печатает, сколько образ занимает от раздела, и проверяет, что он вообще
/// лёг туда, куда должен.
///
/// Проверок две, и обе про то, чего не поймает никто другой.
///
/// Адрес: `base` — минимальный физический адрес сегмента ELF, и он обязан
/// совпасть с началом раздела. Разойтись они могут тише, чем кажется:
/// `-C link-arg=--nmagic` в `crates-cross/.cargo/config.toml` держит заголовки
/// ELF вне первого загружаемого сегмента, а без него линкер вкладывает их
/// внутрь, и `p_paddr` уезжает ниже `ORIGIN`. Флаг лежит в списке, который с
/// OTA никто не связывает, — убери его, и `.bin` получит лишний префикс,
/// каждый байт сместится, а подпись честно подтвердит испорченный образ.
/// Проверку вынесло [`ensure_image_base`]: тем же адресом прошивка
/// сверяется и перед прошивкой в тестах на плате, где подпись уже позади.
///
/// Размер: образ, не помещающийся в раздел, — это ошибка, а не повод для
/// предупреждения. Линкер её поймает не всегда: раскладку `memory.x` могли
/// поправить руками после сборки.
pub fn report_image(base: u64, size: u64) -> Result<(), anyhow::Error> {
    save_image_size(size)?;

    let regions = app_memory_regions()?;
    let Some((name, partition)) = app_region(&regions) else {
        // Раздела приложения в раскладке нет — сверять образ не с чем, а подписывать
        // и раздавать образ без единой проверки нельзя (так же отказывает прошивка перед тестами).
        #[expect(
            clippy::cast_precision_loss,
            reason = "размеры образа и раздела на порядки меньше 2^52"
        )]
        let size_kib = size as f64 / 1024.0;
        println!("app: {size_kib:.1} KiB");
        anyhow::bail!(
            "в crates-cross/app/memory.x нет ни региона ACTIVE, ни FLASH — адрес и размер \
             образа не с чем сверять"
        );
    };

    ensure_image_base(base, name, partition)?;

    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    let percent = size as f64 * 100.0 / partition.length.max(1) as f64;
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    let size_kib = size as f64 / 1024.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    let partition_kib = partition.length as f64 / 1024.0;
    match IMAGE_BUDGET_PERCENT {
        Some(budget) => println!(
            "app: {size_kib:.1} KiB из {partition_kib:.1} KiB {name} ({percent:.1}%, бюджет {budget}%)"
        ),
        None => println!("app: {size_kib:.1} KiB из {partition_kib:.1} KiB {name} ({percent:.1}%)"),
    }
    if let Some(line) = size_delta(baseline_image_size()?, size, partition.length) {
        println!("     {line}");
    }
    anyhow::ensure!(
        size <= partition.length,
        "образ не помещается в раздел {name}: {size} байт против {}",
        partition.length,
    );
    check_image_budget(name, size, partition.length, IMAGE_BUDGET_PERCENT)
}

/// Секции, которые линкер кладёт в регион `RAM` из `memory.x` статически:
/// `.data`, `.bss` и `.uninit` — `cortex-m-rt`, плюс статики с
/// `#[link_section = ".uninit"]`. `.dma_nc`/`.bdma_nc` лежат в своих регионах
/// и сюда не входят.
const STATIC_RAM_SECTIONS: [&str; 3] = [".data", ".bss", ".uninit"];

/// Сколько статической RAM занимает образ: сумма `.data`, `.bss`, `.uninit`.
///
/// Отсутствующая секция — 0, а не ошибка: `.uninit` нет у проекта без
/// `#[link_section = ".uninit"]`-статиков.
///
/// # Errors
///
/// `elf` не разбирается как 32-битный ELF.
pub fn static_ram(elf: &[u8]) -> Result<u64, anyhow::Error> {
    use object::read::elf::ElfFile32;
    use object::read::{Object, ObjectSection};

    let file = ElfFile32::<object::Endianness>::parse(elf).context("разобрать ELF")?;
    Ok(sum_static_ram(file.sections().filter_map(|section| {
        Some((section.name().ok()?.to_owned(), section.size()))
    })))
}

/// Ядро [`static_ram`] над списком `(имя, размер)` — тестируется без ELF:
/// писать ELF в `object` у xtask выключено.
fn sum_static_ram(sections: impl IntoIterator<Item = (String, u64)>) -> u64 {
    sections
        .into_iter()
        .filter(|(name, _)| STATIC_RAM_SECTIONS.contains(&name.as_str()))
        .map(|(_, size)| size)
        .sum()
}

/// Печатает статическую RAM образа против региона `RAM` из `memory.x`.
///
/// Формат строки — контракт: его разбирает `template-check`
/// (`parse_static_ram`), менять только вместе.
pub fn report_ram(elf: &Path) -> Result<(), anyhow::Error> {
    let bytes = fs::read(elf).with_context(|| format!("прочитать {}", elf.display()))?;
    let used = static_ram(&bytes).with_context(|| format!("разобрать {}", elf.display()))?;
    let regions = app_memory_regions()?;
    let Some(ram) = region(&regions, "RAM") else {
        anyhow::bail!("в crates-cross/app/memory.x нет региона RAM");
    };
    println!("{}", ram_line(used, ram.length));
    Ok(())
}

fn ram_line(used: u64, total: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры RAM на порядки меньше 2^52"
    )]
    let percent = used as f64 * 100.0 / total.max(1) as f64;
    format!("ram: {used} байт статической RAM из {total} байт RAM ({percent:.1}%)")
}

/// Занятость зоны bootloader'а (`FLASH` его `memory.x`). Только предупреждение:
/// переполнение поймает линкер, а близость к границе — повод подумать, не отказ.
pub fn report_boot(size: u64) -> Result<(), anyhow::Error> {
    let regions = boot_memory_regions()?;
    let Some(flash) = region(&regions, "FLASH") else {
        anyhow::bail!("в crates-cross/boot/memory.x нет региона FLASH");
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    let (size_kib, zone_kib, percent) = (
        size as f64 / 1024.0,
        flash.length as f64 / 1024.0,
        size as f64 * 100.0 / flash.length.max(1) as f64,
    );
    println!("boot: {size_kib:.1} KiB из {zone_kib:.1} KiB FLASH ({percent:.1}%)");
    if size * 10 >= flash.length * 9 {
        println!("ВНИМАНИЕ: зона bootloader'а занята больше чем на 90%");
    }
    Ok(())
}

/// Bootloader не должен тянуть исполнитель и граф задач: после зависимости
/// `boot` -> `domain` (ради `screen_update`) они могли приехать транзитивно.
/// Предупреждение, не отказ — проверка по именам крейтов в таблице символов.
pub fn warn_boot_runtime_symbols(elf: &Path) -> Result<(), anyhow::Error> {
    use object::read::elf::ElfFile32;
    use object::read::{Object, ObjectSymbol};

    let bytes = fs::read(elf).with_context(|| format!("прочитать {}", elf.display()))?;
    let file = ElfFile32::<object::Endianness>::parse(&*bytes)
        .with_context(|| format!("разобрать {}", elf.display()))?;
    for krate in ["embassy_executor", "supervisor"] {
        let hits = file
            .symbols()
            .filter(|symbol| symbol.name().is_ok_and(|name| name.contains(krate)))
            .count();
        if hits > 0 {
            println!(
                "ВНИМАНИЕ: в ELF bootloader'а {hits} символов {krate} — исполнителю и графу задач \
                 там делать нечего (зависимость boot -> domain потянула лишнее?)"
            );
        }
    }
    Ok(())
}

/// Переменная, из которой `build` берёт размер образа на ветке по умолчанию.
///
/// Заполняет её CI — из кеша, который пишется только на ветке по умолчанию
/// (иначе размер из чужого PR стал бы базой для всех остальных). Локально она
/// не задана, и `build` о дельте молчит.
const BASELINE_ENV: &str = "IMAGE_SIZE_BASELINE";
/// Переменная с путём, куда положить размер этой сборки числом.
///
/// Тоже CI: без неё пришлось бы вылавливать цифру из человекочитаемой строки
/// регулярным выражением прямо в yaml — то есть держать формат вывода
/// контрактом с двумя конфигурациями CI, не имея на это ни одного теста.
const SIZE_OUT_ENV: &str = "IMAGE_SIZE_OUT";

/// Размер образа на ветке по умолчанию, если CI его передал.
///
/// Мусор в переменной — ошибка, а не молчание: пустая дельта выглядела бы
/// ровно как «база не задана», и сломанный шаг CI остался бы незамеченным.
fn baseline_image_size() -> Result<Option<u64>, anyhow::Error> {
    // Разбирается именно «переменной нет», а не любая ошибка чтения: значение
    // не в UTF-8 — это заданная база, которую не прочитали, и молчать о ней
    // нельзя ровно по той же причине, что и о нечисловой.
    let raw = match env::var(BASELINE_ENV) {
        Ok(raw) => raw,
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(env::VarError::NotUnicode(raw)) => {
            let raw = raw.to_string_lossy();
            anyhow::bail!("{BASELINE_ENV}={raw} — не текст, ожидается размер образа в байтах")
        }
    };
    let value = raw
        .trim()
        .parse()
        .with_context(|| format!("{BASELINE_ENV}={raw} — ожидается размер образа в байтах"))?;
    Ok(Some(value))
}

/// Кладёт размер этой сборки туда, куда просит CI.
fn save_image_size(size: u64) -> Result<(), anyhow::Error> {
    let Some(path) = env::var_os(SIZE_OUT_ENV) else {
        return Ok(());
    };
    let path = PathBuf::from(path);
    fs::write(&path, size.to_string())
        .with_context(|| format!("не удалось записать размер образа в {}", path.display()))
}

/// Строка про то, как изменился образ с прошлой сборки на ветке по умолчанию.
///
/// `None` — базы нет, и говорить нечего: так выглядит любая локальная сборка и
/// первый прогон CI после включения этой проверки.
///
/// Порога здесь нет и не будет — как у покрытия. Цифра нужна, чтобы увидеть
/// рост в тот момент, когда автор ещё помнит, зачем он его добавил: flash
/// растёт по паре сотен байт за коммит, и `IMAGE_BUDGET_PERCENT` срабатывает
/// на полсотни коммитов позже, когда искать виноватого уже дорого. Это разные
/// инструменты, а не два порога.
fn size_delta(baseline: Option<u64>, size: u64, partition: u64) -> Option<String> {
    let baseline = baseline?;
    if baseline == size {
        #[expect(
            clippy::cast_precision_loss,
            reason = "размеры образа и раздела на порядки меньше 2^52"
        )]
        return Some(format!(
            "размер не изменился ({:.1} KiB)",
            size as f64 / 1024.0
        ));
    }

    let (sign, diff) = if size > baseline {
        ("+", size - baseline)
    } else {
        ("−", baseline - size)
    };
    // Проценты с десятой долей, в отличие от целой цифры строкой выше: рост в
    // триста байт от раздела в сто килобайт — это треть процента, и на целых
    // числах обе стороны выглядели бы одинаково.
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    let percent = |bytes: u64| bytes as f64 * 100.0 / partition.max(1) as f64;
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    Some(format!(
        "{sign}{diff} байт к ветке по умолчанию (было {:.1} KiB, {:.1}% → {:.1}%)",
        baseline as f64 / 1024.0,
        percent(baseline),
        percent(size),
    ))
}

/// Сколько процентов раздела приложению позволено занять.
///
/// `None` в шаблоне — по той же причине, по которой `None` стоит в
/// `EXPECTED_SYS_HZ` у target-теста тактирования: чип шаблон знает, а сколько
/// места этот конкретный проект хочет оставить про запас — нет. Пока здесь
/// `None`, работает прежнее поведение: не влезло в раздел — ошибка, подошло к
/// девяти десятым — предупреждение.
///
/// Заполните — и `cargo xtask build` начнёт падать, стоит образу перерасти
/// бюджет. Смысл в том, чтобы узнать об этом на сборке, а не при первом OTA:
/// в раздел `DFU` образ ещё влезет, а вот места под следующее обновление уже
/// не останется — и устройство в поле окажется без пути назад. Цифру берут не
/// с потолка, а из запаса на оставшиеся функции; 85 — разумная отправная
/// точка для проекта, который ещё растёт.
///
/// Проверка работает и локально, и в CI одинаково — именно поэтому она
/// константа в коде, а не переменная окружения CI-джоба: порог это свойство
/// проекта, а не свойство запуска.
const IMAGE_BUDGET_PERCENT: Option<u64> = None;

/// Сверка занятого места с бюджетом — отдельно от `report_image`, чтобы её
/// можно было проверить тестом: сам `report_image` читает `memory.x` с диска
/// и без собранного проекта не работает.
fn check_image_budget(
    name: &str,
    size: u64,
    length: u64,
    budget: Option<u64>,
) -> Result<(), anyhow::Error> {
    // Целыми и без промежуточного деления: усечённый процент пропускал бы 85,9 %
    // при бюджете 85 и не сработал бы предупреждение на 89,9 %.
    let length = length.max(1);
    #[expect(
        clippy::cast_precision_loss,
        reason = "размеры образа и раздела на порядки меньше 2^52"
    )]
    let percent = size as f64 * 100.0 / length as f64;
    let Some(budget) = budget else {
        // Порог не задан — остаётся прежняя мягкая подсказка. Не ошибка:
        // проект, сознательно занявший раздел под завязку, имеет на это право,
        // а шаблон не знает, планируется ли рост.
        if size * 10 >= length * 9 {
            println!("ВНИМАНИЕ: до границы раздела осталось меньше десятой части");
        }
        return Ok(());
    };

    anyhow::ensure!(
        size * 100 <= budget * length,
        "образ занял {percent:.1}% раздела {name} при бюджете {budget}% \
         (IMAGE_BUDGET_PERCENT в crates-host/xtask/src/size.rs). Либо прошивка выросла \
         сильнее, чем планировалось, либо бюджет пора пересмотреть — но менять его \
         молча, чтобы сборка позеленела, значит выключить проверку",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Значение шаблона по умолчанию: раздел, занятый под завязку, сборку не
    /// роняет. Иначе первый же проект, которому места хватает впритык,
    /// получал бы красную сборку без единой ошибки в коде.
    #[test]
    fn an_unset_budget_lets_the_image_fill_the_partition() {
        super::check_image_budget("ACTIVE", 99, 100, None).expect("без бюджета проверки нет");
    }

    /// Граница включительно: бюджет 85 означает «до 85 процентов можно», а не
    /// «меньше 85». Иначе цифра в константе значила бы не то, что написано.
    #[test]
    fn a_budget_allows_exactly_its_own_percentage() {
        super::check_image_budget("ACTIVE", 85, 100, Some(85))
            .expect("ровно бюджет — это ещё не сверх");
    }

    #[test]
    fn a_budget_rejects_the_first_percent_over_it() {
        let err = super::check_image_budget("ACTIVE", 86, 100, Some(85))
            .expect_err("перерасход бюджета должен ронять сборку");

        // В сообщении обязаны быть обе цифры и имя раздела: без них человек,
        // увидевший красную сборку, не поймёт, насколько он промахнулся.
        let text = err.to_string();
        assert!(text.contains("86.0"), "нет фактического процента: {text}");
        assert!(text.contains("85"), "нет бюджета: {text}");
        assert!(text.contains("ACTIVE"), "нет имени раздела: {text}");
    }

    /// Усечение процента до сравнения пропускало 85,9 % при бюджете 85.
    #[test]
    fn a_budget_is_not_truncated_before_the_comparison() {
        super::check_image_budget("ACTIVE", 8590, 10_000, Some(85))
            .expect_err("85,9 % — это сверх бюджета 85 %");
        super::check_image_budget("ACTIVE", 8500, 10_000, Some(85)).expect("ровно 85,0 % можно");
    }

    /// Без базы дельты нет — и это нормальный режим, а не поломка: так
    /// выглядит любая локальная сборка и первый прогон CI после включения
    /// проверки.
    #[test]
    fn without_a_baseline_there_is_no_delta() {
        assert!(super::size_delta(None, 13_000, 131_072).is_none());
    }

    /// Рост и уменьшение показываются по-разному, и в обоих случаях видно
    /// «было».
    #[test]
    fn a_delta_says_which_way_the_image_moved() {
        let grew = super::size_delta(Some(12_800), 13_112, 131_072).expect("образ вырос");
        insta::assert_snapshot!(grew, @"+312 байт к ветке по умолчанию (было 12.5 KiB, 9.8% → 10.0%)");

        let shrank = super::size_delta(Some(13_112), 12_800, 131_072).expect("образ уменьшился");
        insta::assert_snapshot!(shrank, @"−312 байт к ветке по умолчанию (было 12.8 KiB, 10.0% → 9.8%)");
    }

    /// Совпадение размеров — тоже ответ, а не молчание: «дельты нет» и «база
    /// не задана» это разные вещи, и различать их должен читатель лога.
    #[test]
    fn an_unchanged_image_says_so() {
        let same = super::size_delta(Some(13_000), 13_000, 131_072).expect("размер тот же");
        insta::assert_snapshot!(same, @"размер не изменился (12.7 KiB)");
    }

    fn sections(list: &[(&str, u64)]) -> Vec<(String, u64)> {
        list.iter()
            .map(|&(name, size)| (name.to_owned(), size))
            .collect()
    }

    #[test]
    fn static_ram_sums_data_bss_and_uninit() {
        let elf = sections(&[
            (".text", 10_000),
            (".data", 100),
            (".bss", 2_000),
            (".uninit", 1_024),
            (".dma_nc", 4_096),
        ]);
        assert_eq!(super::sum_static_ram(elf), 3_124);
    }

    #[test]
    fn missing_sections_count_as_zero() {
        assert_eq!(
            super::sum_static_ram(sections(&[(".text", 10), (".bss", 64)])),
            64
        );
        assert_eq!(super::sum_static_ram(Vec::new()), 0);
    }

    /// Формат читает `template-check` — снапшот держит контракт.
    #[test]
    fn the_ram_line_has_the_fixed_format() {
        insta::assert_snapshot!(
            super::ram_line(3_124, 131_072),
            @"ram: 3124 байт статической RAM из 131072 байт RAM (2.4%)"
        );
    }
}
