//! Прогоняет реальный `chip-select.rhai` через движок Rhai с подменённым
//! `variable::*` — без запуска `cargo generate` и без TTY. `variable::prompt`
//! в моке сам идёт к заранее известной цели (выбирает вариант, префиксом
//! которого является целевой суффикс), так что тест проверяет тот же путь,
//! что реальный интерактивный каскад, а не только `--define chip_feature=...`
//! (та ветка не создаёт ни одного `variable::prompt` вызова и потому не может
//! поймать баги вроде бесконечного цикла в самом каскаде).

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use rhai::{
    AST, Array, Dynamic, Engine, EvalAltResult, Module, Scope,
    module_resolvers::StaticModuleResolver,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("chip-data-gen должен лежать внутри репозитория")
        .to_path_buf()
}

fn script_path() -> PathBuf {
    repo_root().join("chip-select.rhai")
}

fn read_script() -> String {
    std::fs::read_to_string(script_path()).expect("не удалось прочитать chip-select.rhai")
}

/// Список чипов со штампом версий — тот файл, который хук импортирует первым.
fn read_chip_list() -> String {
    std::fs::read_to_string(repo_root().join("chip-data").join("chips.rhai"))
        .expect("не удалось прочитать chip-data/chips.rhai")
}

/// Скрипт хука и таблицы, которые он импортирует из `chip-data/`.
struct Script {
    ast: AST,
    modules: StaticModuleResolver,
}

/// Парсинг — основная стоимость прогона, а таблицы раскладок в `chip-data/`
/// — это ~90 тысяч строк; поэтому и скрипт, и все его модули разбираются один
/// раз на вызывающий тест, а не при каждом обращении к `run_cascade_to` (для
/// `cascade_reaches_every_chip_without_hanging` — 1479 раз). Модули ложатся
/// в резолвер под теми же путями, что пишет `import` (`chip-data/layout/f4`),
/// так что выбор файла семейства проверяется тем же кодом хука.
fn compile_script() -> Script {
    let engine = Engine::new();
    let ast = engine
        .compile(read_script())
        .expect("chip-select.rhai не парсится");

    let root = repo_root();
    let mut modules = StaticModuleResolver::new();
    let mut dirs = vec![root.join("chip-data")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("не удалось прочитать chip-data/")
        {
            let path = entry.expect("элемент каталога").path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            // Резервные копии редактора и прочий мусор рядом — не модули.
            if path.extension().is_none_or(|ext| ext != "rhai") {
                continue;
            }
            let module_ast = engine
                .compile_file(path.clone())
                .unwrap_or_else(|err| panic!("{} не парсится: {err}", path.display()));
            let module = Module::eval_ast_as_new(Scope::new(), &module_ast, &engine)
                .unwrap_or_else(|err| panic!("{} не исполняется: {err}", path.display()));
            let key = path
                .strip_prefix(&root)
                .expect("модуль внутри репозитория")
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            modules.insert(key, module);
        }
    }
    Script { ast, modules }
}

type Vars = Rc<RefCell<HashMap<String, String>>>;
type Files = Rc<RefCell<HashMap<String, Vec<String>>>>;
type Deleted = Rc<RefCell<Vec<String>>>;
/// Всё, что хук напечатал. Сообщения при генерации — единственный способ
/// объяснить пользователю решение, принятое за него (сняли OTA, сняли раздел
/// настроек, не посчитали раскладку), и ошибиться в них так же легко, как в
/// коде: одно из них уже врало (см. `ota_blamed_on_the_settings_partition`).
type Prints = Rc<RefCell<Vec<String>>>;

/// Итог одного прогона: переменные (chip/chip_feature/cpu/target/write_size/
/// ota/bank_mode), файлы, записанные через `file::write` (memory.x), построчно,
/// и пути, снесённые через `file::delete` (crates-cross/boot у чипов без OTA).
struct CascadeResult {
    vars: HashMap<String, String>,
    files: HashMap<String, Vec<String>>,
    deleted: Vec<String>,
    prints: Vec<String>,
}

/// Прогоняет уже скомпилированный chip-select.rhai один раз, ведя каскад к
/// `target_suffix` (суффикс без "stm32", например "f407ve" или "l151c6-a").
fn run_cascade_to(
    script: &Script,
    target_suffix: &str,
) -> Result<CascadeResult, Box<EvalAltResult>> {
    run_cascade_with(script, target_suffix, &[])
}

/// То же, но с заранее выставленными переменными — так проверяются ветки,
/// которые в реальной генерации приходят из `[placeholders]` или `--define`
/// (`ota`, `write_size`, `chip`).
fn run_cascade_with(
    script: &Script,
    target_suffix: &str,
    preset: &[(&str, &str)],
) -> Result<CascadeResult, Box<EvalAltResult>> {
    let vars: Vars = Rc::new(RefCell::new(
        preset
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>(),
    ));
    let files: Files = Rc::new(RefCell::new(HashMap::new()));
    let deleted: Deleted = Rc::new(RefCell::new(Vec::new()));
    let prints: Prints = Rc::new(RefCell::new(Vec::new()));
    let mut engine = Engine::new();
    // Предел операций обязателен: без него `Engine::new()` считает регрессию с
    // бесконечным циклом не ошибкой, а зависанием, и подвешивает пайплайн
    // вместо того, чтобы упасть на тесте. С запасом выше нынешнего расхода на
    // самый длинный каскад, иначе ограничение срабатывало бы на здоровом
    // скрипте.
    engine.set_max_operations(2_000_000);
    engine.set_module_resolver(script.modules.clone());
    {
        let prints = prints.clone();
        engine.on_print(move |line| prints.borrow_mut().push(line.to_string()));
    }

    let mut module = Module::new();
    {
        let vars = vars.clone();
        module.set_native_fn(
            "is_set",
            move |name: &str| -> Result<bool, Box<EvalAltResult>> {
                Ok(vars.borrow().contains_key(name))
            },
        );
    }
    {
        let vars = vars.clone();
        module.set_native_fn(
            "get",
            move |name: &str| -> Result<Dynamic, Box<EvalAltResult>> {
                Ok(vars
                    .borrow()
                    .get(name)
                    .unwrap_or_else(|| panic!("variable::get(\"{name}\") до variable::set"))
                    .clone()
                    .into())
            },
        );
    }
    {
        let vars = vars.clone();
        module.set_native_fn(
            "set",
            move |name: &str, value: Dynamic| -> Result<(), Box<EvalAltResult>> {
                vars.borrow_mut()
                    .insert(name.to_string(), value.to_string());
                Ok(())
            },
        );
    }
    {
        let target = target_suffix.to_string();
        module.set_native_fn(
            "prompt",
            move |_text: &str, _default: Dynamic, choices: Array| -> Result<Dynamic, Box<EvalAltResult>> {
                // Самый длинный подходящий вариант — а не первый: у чипов
                // вроде "l151c6-a" среди choices одновременно есть и
                // "l151c6" ("остановиться здесь"), и "l151c6-" (продолжение),
                // и оба тривиально являются префиксом цели "l151c6-a" —
                // нужно продолжать, а не останавливаться на более коротком.
                let pick = choices
                    .iter()
                    .map(|c| c.clone().into_string().expect("choices — массив строк"))
                    .filter(|choice| target.starts_with(choice.as_str()))
                    .max_by_key(String::len);
                match pick {
                    Some(choice) => Ok(choice.into()),
                    None => Err(format!(
                        "variable::prompt: ни один вариант из {choices:?} не ведёт к цели \"{target}\""
                    )
                    .into()),
                }
            },
        );
    }
    engine.register_static_module("variable", module.into());
    engine.register_fn("abort", |reason: &str| -> Result<(), Box<EvalAltResult>> {
        Err(format!("abort: {reason}").into())
    });

    let mut file_module = Module::new();
    {
        let files = files.clone();
        file_module.set_native_fn(
            "write",
            move |path: &str, content: Array| -> Result<(), Box<EvalAltResult>> {
                let lines = content
                    .iter()
                    .map(|c| {
                        c.clone()
                            .into_string()
                            .expect("file::write(path, content) — content построчно")
                    })
                    .collect();
                files.borrow_mut().insert(path.to_string(), lines);
                Ok(())
            },
        );
    }
    {
        let deleted = deleted.clone();
        file_module.set_native_fn(
            "delete",
            move |path: &str| -> Result<(), Box<EvalAltResult>> {
                deleted.borrow_mut().push(path.to_string());
                Ok(())
            },
        );
    }
    engine.register_static_module("file", file_module.into());

    let mut scope = Scope::new();
    engine.run_ast_with_scope(&mut scope, &script.ast)?;

    Ok(CascadeResult {
        vars: vars.borrow().clone(),
        files: files.borrow().clone(),
        deleted: deleted.borrow().clone(),
        prints: prints.borrow().clone(),
    })
}

/// Полный список суффиксов из CHIPS — читает тот же файл, что импортирует
/// сам хук, не дублирует данные руками.
fn all_chip_suffixes() -> Vec<String> {
    let script = read_chip_list();
    let begin = script.find("const CHIPS = [").expect("CHIPS не найден");
    let end = script[begin..].find("];").expect("конец CHIPS не найден") + begin;
    script[begin..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            line.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn cascade_reaches_every_chip_without_hanging() {
    let suffixes = all_chip_suffixes();
    assert!(
        suffixes.len() > 1000,
        "подозрительно короткий список чипов: {}",
        suffixes.len()
    );
    let ast = compile_script();

    let mut failures = Vec::new();
    for suffix in &suffixes {
        match run_cascade_to(&ast, suffix) {
            Ok(result) => {
                let got = result.vars.get("chip_feature").map(String::as_str);
                if got != Some(&*format!("stm32{suffix}")) {
                    failures.push(format!(
                        "{suffix}: chip_feature = {got:?}, ожидался stm32{suffix}"
                    ));
                }
            }
            Err(err) => failures.push(format!("{suffix}: {err}")),
        }
    }

    assert!(
        failures.is_empty(),
        "каскад не дошёл до цели для {} чипов из {}:\n{}",
        failures.len(),
        suffixes.len(),
        failures.join("\n")
    );
}

#[test]
fn cascade_stops_at_chip_that_is_a_prefix_of_another() {
    // "l151c6" — валидный чип сам по себе, но и префикс "l151c6-a". Раньше
    // здесь был баг: выбор "остановиться здесь" не завершал цикл.
    let result = run_cascade_to(&compile_script(), "l151c6").expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("chip_feature").map(String::as_str),
        Some("stm32l151c6")
    );
    assert_eq!(
        result.vars.get("chip").map(String::as_str),
        Some("STM32L151C6")
    );
}

#[test]
fn cascade_reaches_the_longer_sibling_too() {
    let result = run_cascade_to(&compile_script(), "l151c6-a").expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("chip_feature").map(String::as_str),
        Some("stm32l151c6-a")
    );
    // Более точная цель probe-rs из PACKAGE_CHOICES, не общая "STM32L151C6".
    assert_eq!(
        result.vars.get("chip").map(String::as_str),
        Some("STM32L151C6TxA")
    );
    // Дефис здесь — силиконовая градация ("-a"), не ядро: is_dual_core()
    // должна отличать этот случай от "-cm4"/"-cm7" и т.п.
    assert_eq!(
        result.vars.get("dual_core").map(String::as_str),
        Some("false")
    );
}

#[test]
fn dual_core_suffix_uses_core_override_not_family_default() {
    // "h7" по family_table() даёт cortex-m7, но "-cm4" должен явно
    // перебивать это через core_override().
    let result = run_cascade_to(&compile_script(), "h745zi-cm4").expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("cpu").map(String::as_str),
        Some("cortex-m4")
    );
    assert_eq!(
        result.vars.get("target").map(String::as_str),
        Some("thumbv7em-none-eabihf")
    );
    assert_eq!(
        result.vars.get("chip").map(String::as_str),
        Some("STM32H745ZI")
    );
    assert_eq!(
        result.vars.get("dual_core").map(String::as_str),
        Some("true")
    );
}

#[test]
fn single_core_suffix_leaves_dual_core_false() {
    let result = run_cascade_to(&compile_script(), "f407ve").expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("dual_core").map(String::as_str),
        Some("false")
    );
}

/// Признак «стёртая страница читается нулями» идёт из метаданных, а не из
/// имени чипа.
///
/// `wb10cc` и `wb15cc` здесь — не для полноты. Пока признак считался как
/// «фича начинается на `l0`/`l1`», эти два чипа проходили мимо: `erase_value`
/// у них тот же ноль, а префикс совсем другой. Сгенерированный под них проект
/// получал `embassy-boot` без `flash-erase-zero` (обмен разделов читает
/// признак «стёрто» наоборот) и раздел настроек, который не работает вовсе, —
/// обе поломки молчаливые.
///
/// `wb55rg` рядом с ними не для симметрии: без него тест остался бы зелёным и
/// у реализации «всё семейство WB стирается в ноль», которая сломала бы
/// полсотни рабочих чипов.
#[test]
fn erase_zero_follows_the_metadata_not_the_chip_name() {
    let ast = compile_script();
    for (suffix, expected) in [
        ("l073rz", "true"),
        ("l151c6", "true"),
        ("wb10cc", "true"),
        ("wb15cc", "true"),
        ("f407ve", "false"),
        ("wb55rg", "false"),
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("erase_zero").map(String::as_str),
            Some(expected),
            "{suffix}: erase_zero разошёлся с erase_value в stm32-metapac",
        );
    }
}

/// Чип, куда OTA не помещается и без раздела настроек, получает при генерации
/// настоящую причину отказа, а не совет отказаться от настроек: пока такой
/// совет был, на f030c8 (15K под ACTIVE при OTA_MIN_ACTIVE 24K) он печатался
/// ложно и глушил настоящую причину.
#[test]
fn ota_refusal_names_the_real_reason_with_settings() {
    let ast = compile_script();
    let too_small = run_cascade_with(
        &ast,
        "f030c8",
        &[("config", "internal"), ("ota", "internal")],
    )
    .expect("каскад не должен падать");
    let log = too_small.prints.join("\n");
    assert!(
        log.contains("OTA отключён"),
        "настоящая причина осталась незаявленной:\n{log}"
    );
}

/// Текст записанного `file::write` файла одной строкой.
fn written(result: &CascadeResult, path: &str) -> String {
    result
        .files
        .get(path)
        .unwrap_or_else(|| panic!("{path} не записан"))
        .join("\n")
}

/// ORIGIN региона `name` из текста memory.x (`None`, если региона нет).
fn region_origin(memory_x: &str, name: &str) -> Option<String> {
    memory_x
        .lines()
        .find(|line| line.trim_start().starts_with(&format!("{name} ")))
        .and_then(|line| line.split("ORIGIN = ").nth(1))
        .and_then(|rest| rest.split(',').next())
        .map(str::to_string)
}

#[test]
fn memory_layout_is_written_when_available() {
    // f407ve — чип с неравномерными секторами (см. commit про BANK1_REGION),
    // у него MEMORY_LAYOUT точно должен быть посчитан.
    let result = run_cascade_to(&compile_script(), "f407ve").expect("каскад не должен падать");
    assert_eq!(result.vars.get("write_size").map(String::as_str), Some("4"));
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("true"));

    // У приложения FLASH — это ACTIVE, а не весь чип: с базы flash стартует
    // bootloader. Раньше оба крейта получали одинаковый memory.x, и
    // `cargo xtask flash` (сначала boot, потом app) затирал bootloader.
    let app = written(&result, "crates-cross/app/memory.x");
    assert!(
        app.contains("FLASH             (rx)  : ORIGIN = 0x08020000, LENGTH = 128K"),
        "{app}"
    );
    assert!(
        app.contains("DFU               (rx)  : ORIGIN = 0x08040000, LENGTH = 256K"),
        "{app}"
    );
    assert!(
        region_origin(&app, "ACTIVE").is_none(),
        "у app отдельного региона ACTIVE быть не должно — это его же FLASH:\n{app}"
    );

    // У bootloader'а FLASH — только его собственная зона, до BOOTLOADER_STATE:
    // переполнение должен ловить линкер, а не молчаливое наложение на ACTIVE.
    // STATE — в последнем 16-килобайтном секторе (3), сектор 4 до ACTIVE пуст.
    let boot = written(&result, "crates-cross/boot/memory.x");
    assert!(
        boot.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 48K"),
        "{boot}"
    );
    assert!(
        boot.contains("BOOTLOADER_STATE  (rx)  : ORIGIN = 0x0800C000, LENGTH = 16K"),
        "{boot}"
    );
    assert!(
        boot.contains("ACTIVE            (rx)  : ORIGIN = 0x08020000, LENGTH = 128K"),
        "{boot}"
    );
}

#[test]
fn common_boards_keep_their_ota_layout() {
    // Ходовые чипы с отладочных плат, покрывающие все классы геометрии flash:
    // 1 KiB сектор (F0/F1), 256 байт (L1), 2 KiB (L0/L4/G0/F3), 4 KiB (G4),
    // неравномерные сектора до 128 KiB (F4/F7/H7). Регрессия, которую тест
    // ловит: предел роста резерва задавался абсолютным числом страниц, а
    // исходный резерв у мелкосекторных чипов сам по себе занимает десятки
    // страниц (32 KiB при секторе 256 байт — 128 штук), и вся мелкосекторная
    // половина линейки разом теряла OTA.
    //
    // Все — с flash заведомо больше резерва под bootloader: у 32-килобайтных
    // (f030c6, l151c6) OTA не помещается по-честному, это не регрессия.
    //
    // Отсюда убраны f030r8 и f051r8 — тоже ходовые Nucleo, но с 64 KiB flash:
    // под приложение у них остаётся 15 KiB, куда не влезает даже пустая
    // прошивка (проверено сборкой — переполнение на 848 байт). OTA им теперь
    // не предлагается вовсе, и стережёт это
    // `ota_is_dropped_where_the_app_partition_is_too_small`.
    let ast = compile_script();
    for suffix in [
        "f103rb", "f303vc", "l073rz", "l151cb", "l432kc", "l476rg", "g071rb", "g474re", "f407ve",
        "f411re", "f746zg", "h743zi",
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("ota").map(String::as_str),
            Some("true"),
            "{suffix} остался без OTA: {:?}",
            result.files.get("crates-cross/app/memory.x")
        );
    }
}

#[test]
fn memory_x_lists_every_region_of_the_chip() {
    let ast = compile_script();

    // H723VE: кроме 128 KiB DTCM (это и есть RAM) у чипа есть ещё 416 KiB —
    // раньше memory.x про них молчал, и пользователь видел только DTCM.
    let app = written(
        &run_cascade_to(&ast, "h723ve").expect("каскад не должен падать"),
        "crates-cross/app/memory.x",
    );
    for region in ["ITCM", "AXISRAM", "SRAM2", "SRAM3", "SRAM4"] {
        assert!(
            region_origin(&app, region).is_some(),
            "{region} не выведен:\n{app}"
        );
    }

    // F407VE: CCMRAM (отдельный физический блок, не алиас) и OTP.
    let app = written(
        &run_cascade_to(&ast, "f407ve").expect("каскад не должен падать"),
        "crates-cross/app/memory.x",
    );
    assert!(region_origin(&app, "CCMRAM").is_some(), "{app}");
    assert!(region_origin(&app, "OTP").is_some(), "{app}");

    // G474RE: CCMRAM_ICODE — второе окно того же блока, что CCMRAM_DCODE,
    // который уже внутри RAM. Объявить его рабочей памятью — значит дать
    // разместить данные дважды по одному физическому адресу, чего линкер не
    // поймает: выводим закомментированным.
    let app = written(
        &run_cascade_to(&ast, "g474re").expect("каскад не должен падать"),
        "crates-cross/app/memory.x",
    );
    assert!(
        region_origin(&app, "CCMRAM_ICODE").is_none(),
        "алиас не должен быть объявлен регионом:\n{app}"
    );
    assert!(
        app.contains("/* CCMRAM_ICODE"),
        "алиас должен быть упомянут закомментированным:\n{app}"
    );

    // H503CB: окна внешних шин (FMC/OCTOSPI/SDRAM) — памяти за ними нет, пока
    // микросхема не распаяна, поэтому тоже только комментарием.
    let app = written(
        &run_cascade_to(&ast, "h503cb").expect("каскад не должен падать"),
        "crates-cross/app/memory.x",
    );
    for region in ["FMC_BANK_1", "OCTOSPI_BANK_1", "SDRAM_BANK_1"] {
        assert!(
            region_origin(&app, region).is_none(),
            "{region} внешний, объявлять его регионом нельзя:\n{app}"
        );
        assert!(app.contains(region), "{region} не упомянут вовсе:\n{app}");
    }
    // А BKPSRAM у того же чипа — настоящая внутренняя память.
    assert!(region_origin(&app, "BKPSRAM").is_some(), "{app}");
}

#[test]
fn multi_config_chip_selects_a_bank_mode() {
    // У g474re в stm32-metapac две карты памяти, и build.rs embassy-stm32 без
    // явной фичи паникует — то есть проект не собрался бы вовсе.
    let result = run_cascade_to(&compile_script(), "g474re").expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("bank_mode").map(String::as_str),
        Some("single-bank")
    );

    // У одноконфигурационного чипа фичи быть не должно: embassy-stm32
    // паникует и на лишней ("The 'single-bank' feature is not supported on
    // this dual bank chip").
    let result = run_cascade_to(&compile_script(), "f407ve").expect("каскад не должен падать");
    assert_eq!(result.vars.get("bank_mode").map(String::as_str), Some(""));
}

#[test]
fn chip_without_room_for_ota_gets_single_image_layout() {
    // h723ve: 512 KiB flash одним регионом с сектором 128 KiB — всего 4
    // сектора, а схеме нужно минимум 5 (BOOTLOADER + STATE + ACTIVE + 2×DFU).
    let result = run_cascade_to(&compile_script(), "h723ve").expect("каскад не должен падать");
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("false"));

    let app = written(&result, "crates-cross/app/memory.x");
    assert!(
        app.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 512K"),
        "{app}"
    );
    for region in ["BOOTLOADER_STATE", "ACTIVE", "DFU"] {
        assert!(
            region_origin(&app, region).is_none(),
            "без OTA региона {region} быть не должно:\n{app}"
        );
    }
    assert!(
        app.contains("OTA (crates-cross/boot) не подключён"),
        "причина должна остаться в самом memory.x:\n{app}"
    );
    // Каталог bootloader'а сносится самим хуком: [conditional] в
    // cargo-generate.toml переменных из хука не видит (проверено эмпирически).
    assert!(
        result.deleted.contains(&"crates-cross/boot".to_string()),
        "crates-cross/boot должен быть удалён, а удалено: {:?}",
        result.deleted
    );
    assert!(
        !result.files.contains_key("crates-cross/boot/memory.x"),
        "в удалённый каталог писать нельзя — cargo generate падает с os error 3"
    );
}

#[test]
fn memory_layout_invariants_hold_for_every_chip() {
    // Один проход по всем чипам вместо точечных случаев — здесь ловятся сразу
    // три класса регрессий, каждый из которых уже случался:
    //   1. BOOTLOADER_STATE с самого начала flash: на чипах, где первый регион
    //      целиком занимает PAGE_SIZE (H7 — 128 KiB с адреса 0), под сам
    //      бинарник bootloader'а не оставалось места;
    //   2. одинаковый memory.x у app и boot: приложение линковалось с базы
    //      flash и при прошивке затирало bootloader;
    //   3. регионы OTA в проекте, который собирается без bootloader'а
    //      (`__bootloader_*` некому определить — crates-cross/boot туда не входит).
    let ast = compile_script();
    let mut with_ota = 0;
    let mut without_ota = 0;
    let mut failures = Vec::new();
    let mut without_layout = Vec::new();
    for suffix in all_chip_suffixes() {
        let result = match run_cascade_to(&ast, &suffix) {
            Ok(result) => result,
            Err(err) => {
                failures.push(format!("{suffix}: каскад упал: {err}"));
                continue;
            }
        };
        let Some(app) = result.files.get("crates-cross/app/memory.x") else {
            // Раскладки нет — проект не собрался бы без ручного заполнения.
            // Хук на этом месте уже делает `abort` (попадает в `failures`
            // выше), а `chip-data-gen` — `ensure!`; эта ветка — страховка на
            // случай, если хук снова научат молча пропускать memory.x.
            without_layout.push(suffix);
            continue;
        };
        let app = app.join("\n");
        let target_tests = result
            .files
            .get("crates-cross/target-tests/memory.x")
            .map(|lines| lines.join("\n"));
        let ota = result.vars.get("ota").map(String::as_str);
        let mut fail = |what: &str| failures.push(format!("{suffix}: {what}"));

        // Четвёртая регрессия того же класса: этот файл каскад когда-то не
        // писал вовсе, и `cargo xtask test target` не линковался ни на одном
        // чипе — в memory.x оставались шаблонные `ORIGIN = /* 0xXXXXXXXX */`.
        // Тесты на устройстве прошиваются на место приложения, поэтому и
        // раскладка у них ровно та же.
        if target_tests.as_deref() != Some(app.as_str()) {
            fail("target-tests/memory.x должен повторять app/memory.x");
        }

        // Регион обязателен, а не опционален: паникёр release-профиля
        // (`panic-persist`) пишет по символам `_panic_dump_*`, и без них
        // release-образ не линкуется. Раньше его не получали 16 чипов с
        // 2 KiB RAM (STM32L011 и родня) — регион не выводился, если RAM
        // меньше 4 KiB, и на этих чипах генерация давала заведомо
        // несобираемый проект.
        let panic = region_origin(&app, "PANIC");
        if panic.is_none() {
            fail("нет региона PANIC — panic-persist не слинкуется");
        }
        if !app.contains("_panic_dump_start = ORIGIN(PANIC);") {
            fail("нет символов panic-persist");
        }
        if app.contains("PERSIST") || app.contains("_hw_ram_start") {
            fail("в memory.x остались PERSIST или _hw_ram_start — у них больше нет потребителей");
        }
        // Ровно поэтому регион отдаётся символами: своя секция в конце RAM
        // убеждает flip-link, что двигать стек некуда, и он оставляет
        // `_stack_start` в её начале. Прошивка тогда уходит в HardFault на
        // первом же push — поймано на STM32F3Discovery, когда генерация
        // выдавала `.persist (NOLOAD) ... > PERSIST`.
        if app.contains("SECTIONS") {
            fail("в memory.x появилась SECTIONS-директива — она ломает flip-link");
        }

        let Some(app_flash) = region_origin(&app, "FLASH") else {
            fail("в app/memory.x нет региона FLASH");
            continue;
        };

        if ota == Some("true") {
            with_ota += 1;
            let boot = written(&result, "crates-cross/boot/memory.x");
            let boot_state = region_origin(&boot, "BOOTLOADER_STATE");
            let boot_active = region_origin(&boot, "ACTIVE");
            if region_origin(&boot, "FLASH") == boot_state {
                fail(
                    "BOOTLOADER_STATE начинается с самого начала flash — bootloader'у негде лежать",
                );
            }
            if boot_active.as_deref() != Some(app_flash.as_str()) {
                fail(&format!(
                    "app линкуется по {app_flash}, а ACTIVE у bootloader'а — {boot_active:?}"
                ));
            }
        } else {
            without_ota += 1;
            for region in ["BOOTLOADER_STATE", "ACTIVE", "DFU"] {
                if region_origin(&app, region).is_some() {
                    fail(&format!("без OTA не должно быть региона {region}"));
                }
            }
        }
    }
    assert!(
        with_ota > 500 && without_ota > 100,
        "подозрительное распределение: с OTA {with_ota}, без OTA {without_ota}"
    );
    assert!(
        without_layout.is_empty(),
        "у {} чипов не посчитана раскладка memory.x — они остались бы плейсхолдером: {}",
        without_layout.len(),
        without_layout
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(
        failures.is_empty(),
        "нарушены инварианты memory.x у {} чипов:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// LENGTH региона `name` из текста memory.x в байтах (`None` — региона нет или
/// длина записана не как `<число>K`/`<число>M`/`<число>`).
fn region_length(memory_x: &str, name: &str) -> Option<u64> {
    let line = memory_x
        .lines()
        .find(|line| line.trim_start().starts_with(&format!("{name} ")))?;
    let text = line.split("LENGTH = ").nth(1)?.trim();
    let (digits, unit) = match text.chars().last()? {
        'K' => (&text[..text.len() - 1], 1024),
        'M' => (&text[..text.len() - 1], 1024 * 1024),
        _ => (text, 1),
    };
    digits.parse::<u64>().ok().map(|n| n * unit)
}

/// `Partition::new` (embassy-embedded-hal) паникует на старте, если длина
/// раздела не кратна `ERASE_SIZE` флеша под ним. Для `BOOTLOADER_STATE` и
/// `CONFIG` эту страницу задают `state_erase_size`/`config_erase_size` — здесь
/// кратность сверяется с длиной региона в memory.x у всех чипов, с настройками
/// и без.
#[test]
fn state_and_config_lengths_are_multiples_of_their_erase_size() {
    let ast = compile_script();
    let mut failures = Vec::new();
    let mut checked = 0;
    for suffix in all_chip_suffixes() {
        for preset in [&[][..], &[("config", "internal")][..]] {
            let Ok(result) = run_cascade_with(&ast, &suffix, preset) else {
                continue; // падения каскада ловят соседние тесты
            };
            let Some(app) = result.files.get("crates-cross/app/memory.x") else {
                continue;
            };
            let app = app.join("\n");
            for (region, var) in [
                ("BOOTLOADER_STATE", "state_erase_size"),
                ("CONFIG", "config_erase_size"),
            ] {
                let Some(length) = region_length(&app, region) else {
                    continue;
                };
                let Some(page) = result
                    .vars
                    .get(var)
                    .and_then(|v| v.replace('_', "").parse::<u64>().ok())
                else {
                    continue; // страница не число (ручная ветка) — сверять нечем
                };
                checked += 1;
                if page == 0 || length % page != 0 {
                    failures.push(format!(
                        "{suffix} {preset:?}: {region} {length} / {var} {page}"
                    ));
                }
            }
        }
    }
    assert!(checked > 1000, "подозрительно мало проверок: {checked}");
    assert!(
        failures.is_empty(),
        "длина раздела не кратна странице у {}:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Списки чипов в `chip-data/` собраны из чип-фич конкретной версии
/// `embassy-stm32`. Связь «подняли версию — перегенерируйте» до появления
/// штампа держалась только на памяти мейнтейнера, а поднимает версию обычно
/// dependabot/renovate, и молча: списки при этом остались бы от старой версии,
/// а расхождение вылезло бы у пользователя шаблона при генерации.
///
/// Штамп пишет `chip-data-gen` (см. `format_source_stamp`); строка здесь
/// продублирована намеренно — тест это отдельный крейт и до констант бинарника
/// не дотягивается.
#[test]
fn generated_blocks_match_the_declared_embassy_version() {
    let manifest_path = repo_root().join("crates-cross").join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .expect("не удалось прочитать crates-cross/Cargo.toml");
    let declared = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("embassy-stm32"))
        .and_then(|line| line.split_once("version"))
        .and_then(|(_, rest)| rest.trim_start().strip_prefix('='))
        .and_then(|rest| rest.trim_start().strip_prefix('"'))
        .and_then(|rest| rest.split_once('"').map(|(version, _)| version.to_owned()))
        .expect("в crates-cross/Cargo.toml не нашлась версия embassy-stm32");

    let expected = format!("// Источник: embassy-stm32 {declared} (crates-cross/Cargo.toml)");
    assert!(
        read_chip_list().contains(&expected),
        "chip-data/ собран не под embassy-stm32 {declared} — перегенерируйте списки:\n\
         cargo run --manifest-path chip-data-gen/Cargo.toml"
    );
}

#[test]
fn declining_ota_gives_a_single_image_even_when_it_would_fit() {
    // f407ve — чип, где схема помещается: при обычной генерации он получает
    // bootloader и разделы. Ответ `no` на вопрос про OTA должен перевесить.
    let ast = compile_script();
    let with_ota = run_cascade_to(&ast, "f407ve").expect("каскад не должен падать");
    assert_eq!(with_ota.vars.get("ota").map(String::as_str), Some("true"));

    for answer in ["no"] {
        let result = run_cascade_with(&ast, "f407ve", &[("ota", answer)])
            .unwrap_or_else(|err| panic!("каскад с ota={answer} упал: {err}"));

        assert_eq!(
            result.vars.get("ota").map(String::as_str),
            Some("false"),
            "ответ {answer:?} должен отключать OTA"
        );
        assert!(
            result.deleted.contains(&"crates-cross/boot".to_string()),
            "bootloader должен быть удалён, а удалено: {:?}",
            result.deleted
        );
        assert!(
            result
                .deleted
                .contains(&"crates-cross/bsp/src/flash.rs".to_string()),
            "модуль flash без OTA и настроек должен быть удалён, а удалено: {:?}",
            result.deleted
        );

        // Приложению достаётся весь flash, а не раздел ACTIVE, и регионов
        // OTA-схемы в memory.x не остаётся.
        let app = written(&result, "crates-cross/app/memory.x");
        assert!(
            app.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 512K"),
            "{app}"
        );
        for region in ["BOOTLOADER_STATE", "ACTIVE", "DFU"] {
            assert!(
                region_origin(&app, region).is_none(),
                "без OTA региона {region} быть не должно:\n{app}"
            );
        }
        assert!(
            !result.files.contains_key("crates-cross/boot/memory.x"),
            "в удалённый каталог писать нельзя"
        );
    }
}

/// Незнакомый ответ — ошибка генерации, а не «без OTA»: опечатка в
/// `--define` молча сняла бы bootloader. Старые `yes`/`true`/`false` тоже
/// незнакомы — варианты вопроса теперь `internal`/`external`/`no`.
#[test]
fn an_unknown_ota_answer_is_an_error() {
    let ast = compile_script();
    for answer in ["yes", "true", "false", "interal"] {
        let err = run_cascade_with(&ast, "f407ve", &[("ota", answer)])
            .err()
            .unwrap_or_else(|| panic!("ota={answer} должен быть ошибкой"));
        assert!(
            err.to_string().contains("internal, external или no"),
            "{err}"
        );
    }
}

/// То же для `config`: варианты те же, что у `ota`, и `yes` среди них нет.
#[test]
fn an_unknown_config_answer_is_an_error() {
    let ast = compile_script();
    for answer in ["yes", "true", "flash"] {
        let err = run_cascade_with(&ast, "f407ve", &[("config", answer)])
            .err()
            .unwrap_or_else(|| panic!("config={answer} должен быть ошибкой"));
        assert!(
            err.to_string().contains("internal, external или no"),
            "{err}"
        );
    }
}

#[test]
fn accepting_ota_still_loses_to_a_chip_that_cannot_fit_it() {
    // Обратная сторона: `yes` — это «не возражаю», а не «сделай во что бы то
    // ни стало». h723ve схему не вмещает, и ответ ничего не меняет.
    let result = run_cascade_with(&compile_script(), "h723ve", &[("ota", "internal")])
        .expect("каскад не должен падать");
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("false"));
    assert!(result.deleted.contains(&"crates-cross/boot".to_string()));
}

/// `ota=external`: внутри только bootloader и `ACTIVE`, а `STATE`/`DFU` —
/// символами-смещениями на внешней флеши, без регионов в `MEMORY`. Там же,
/// где внутренней схеме места нет (h723ve), внешняя помещается.
#[test]
fn external_ota_keeps_only_the_bootloader_and_active_inside() {
    let ast = compile_script();
    for chip in ["f407ve", "h723ve"] {
        let result = run_cascade_with(&ast, chip, &[("ota", "external")])
            .unwrap_or_else(|err| panic!("{chip}: каскад упал: {err}"));
        assert_eq!(
            result.vars.get("ota").map(String::as_str),
            Some("true"),
            "{chip}"
        );
        assert_eq!(
            result.vars.get("ota_store").map(String::as_str),
            Some("external"),
            "{chip}"
        );
        assert_eq!(
            result.vars.get("page_size").map(String::as_str),
            Some(""),
            "{chip}"
        );
        assert_eq!(
            result.vars.get("ext_page_size").map(String::as_str),
            Some("131_072"),
            "{chip}"
        );

        let app = written(&result, "crates-cross/app/memory.x");
        assert!(
            app.contains("FLASH             (rx)  : ORIGIN = 0x08020000, LENGTH = 384K"),
            "{chip}:\n{app}"
        );
        for region in ["DFU", "BOOTLOADER_STATE"] {
            assert!(
                region_origin(&app, region).is_none(),
                "{chip}: {region}\n{app}"
            );
        }
        assert!(
            app.contains("__bootloader_state_start = 0x00000000;"),
            "{chip}:\n{app}"
        );
        assert!(
            app.contains("__bootloader_dfu_start   = 0x00020000;"),
            "{chip}:\n{app}"
        );

        let boot = written(&result, "crates-cross/boot/memory.x");
        assert!(
            boot.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 128K"),
            "{chip}:\n{boot}"
        );
        assert!(
            boot.contains("__bootloader_dfu_end     = 0x00020000 + 512K;"),
            "{chip}:\n{boot}"
        );
    }
}

/// Настройки во внутреннем флеше при внешней OTA (`config=internal`): на F4 — в
/// мелких секторах зоны bootloader'а, `ACTIVE` при этом не короче; настройки
/// на внешней флеши (`config=external`) — символами за `DFU`.
#[test]
fn external_ota_places_settings_where_the_project_asked() {
    let ast = compile_script();

    let inside = run_cascade_with(
        &ast,
        "f407ve",
        &[("ota", "external"), ("config", "internal")],
    )
    .expect("каскад с config=internal");
    assert_eq!(
        inside.vars.get("config_store").map(String::as_str),
        Some("flash")
    );
    assert_eq!(
        inside.vars.get("config_erase_size").map(String::as_str),
        Some("16_384")
    );
    let app = written(&inside, "crates-cross/app/memory.x");
    assert!(
        app.contains("CONFIG            (rx)  : ORIGIN = 0x08008000, LENGTH = 32K"),
        "{app}"
    );
    assert!(app.contains("LENGTH = 384K"), "{app}");
    let boot = written(&inside, "crates-cross/boot/memory.x");
    assert!(
        boot.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 32K"),
        "bootloader кончается там, где начинаются настройки:\n{boot}"
    );

    let outside = run_cascade_with(
        &ast,
        "f407ve",
        &[("ota", "external"), ("config", "external")],
    )
    .expect("каскад с config=external");
    assert_eq!(
        outside.vars.get("config_store").map(String::as_str),
        Some("external")
    );
    let app = written(&outside, "crates-cross/app/memory.x");
    assert!(region_origin(&app, "CONFIG").is_none(), "{app}");
    assert!(app.contains("__config_start = 0x000A0000;"), "{app}");
}

/// Внутренняя схема не помещается, внешняя — да: хук подсказывает
/// `ota=external`, а не просто снимает OTA.
#[test]
fn internal_ota_that_does_not_fit_suggests_the_external_one() {
    let result = run_cascade_with(&compile_script(), "h723ve", &[("ota", "internal")])
        .expect("каскад не должен падать");
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("false"));
    assert!(
        result
            .prints
            .iter()
            .any(|line| line.contains("ota=external")),
        "{:?}",
        result.prints
    );
}

/// Граф задач снимается там, где раздел приложения слишком мал.
///
/// Точка замера не выдумана: на l011f4 (16 KiB flash, Cortex-M0+) сборка с
/// графом падает у линкера — не хватает двух сотен байт. Порог в
/// `GRAPH_MIN_PARTITION` выбран по этому и по обратному случаю (c011d6, 32 KiB,
/// занимает 39% с графом), так что тест стережёт обе стороны решения.
#[test]
fn graph_is_dropped_when_the_app_partition_is_too_small() {
    let script = compile_script();

    let tiny = run_cascade_to(&script, "l011f4").expect("каскад не должен падать");
    assert_eq!(
        tiny.vars.get("graph").map(String::as_str),
        Some("false"),
        "в 16 KiB граф не помещается — хук обязан его снять"
    );

    let roomy = run_cascade_to(&script, "c011d6").expect("каскад не должен падать");
    assert_eq!(
        roomy.vars.get("graph").map(String::as_str),
        Some("true"),
        "в 32 KiB граф помещается с запасом — снимать его не за что"
    );
}

/// Без ответов про протокол проект — без протокола: ни узла, ни заглушки
/// UART в `bsp`.
#[test]
fn no_protocol_by_default() {
    let result = run_cascade_to(&compile_script(), "f407ve").expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("protocol").map(String::as_str),
        Some("none")
    );
    assert_eq!(
        result.vars.get("interface").map(String::as_str),
        Some("none")
    );
    assert!(
        result
            .deleted
            .iter()
            .any(|path| path == "crates-cross/bsp/src/serial.rs"),
        "заглушка UART без интерфейса uart: {:?}",
        result.deleted
    );
    for path in [
        "crates-host/proto",
        "crates-host/canopen",
        "crates-host/ports/src/link.rs",
        "crates-host/ports/src/modbus.rs",
        "crates-host/ports/src/modbus",
        "crates-host/ports/src/host.rs",
        "crates-host/ports/src/can.rs",
        "crates-host/adapters/src/modbus",
        "crates-host/adapters/src/cobs.rs",
        "crates-host/adapters/src/serial.rs",
        "crates-host/adapters/src/can.rs",
        "crates-host/domain/src/modbus",
        "crates-host/domain/src/host",
        "crates-host/domain/src/canopen.rs",
        "crates-host/domain/src/canopen",
        "crates-host/domain/src/net.rs",
        "crates-host/adapters/src/net.rs",
        "crates-host/adapters/src/net",
        "crates-cross/bsp/src/net.rs",
        "docs/modules/bsp-net.md",
        "crates-cross/bsp/src/usb.rs",
        "crates-host/adapters/src/usb.rs",
        "crates-host/domain/src/usb.rs",
        "docs/modules/bsp-usb.md",
    ] {
        assert!(
            result.deleted.iter().any(|deleted| deleted == path),
            "{path} без своего протокола остался: {:?}",
            result.deleted
        );
    }
}

/// Протокол поверх UART оставляет в `bsp` модуль заглушки.
#[test]
fn protocol_over_uart_keeps_the_serial_module() {
    for protocol in ["modbus", "protobuf"] {
        let result = run_cascade_with(
            &compile_script(),
            "g474re",
            &[("protocol", protocol), ("interface", "uart")],
        )
        .expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("protocol").map(String::as_str),
            Some(protocol)
        );
        assert_eq!(
            result.vars.get("interface").map(String::as_str),
            Some("uart")
        );
        assert!(
            !result
                .deleted
                .iter()
                .any(|path| path == "crates-cross/bsp/src/serial.rs"),
            "{protocol}: заглушка UART удалена"
        );
        // Стек — только выбранного протокола: modbus поверх uart не тянет ни
        // protobuf, ни CAN, и наоборот.
        let kept = |path: &str| !result.deleted.iter().any(|deleted| deleted == path);
        for (path, wanted) in [
            ("crates-host/proto", protocol == "protobuf"),
            ("crates-host/domain/src/host.rs", protocol == "protobuf"),
            ("crates-host/adapters/src/cobs.rs", protocol == "protobuf"),
            ("crates-host/domain/src/modbus.rs", protocol == "modbus"),
            ("crates-host/ports/src/modbus.rs", protocol == "modbus"),
            ("crates-host/ports/src/modbus", protocol == "modbus"),
            ("crates-host/ports/src/link.rs", true),
            ("crates-host/canopen", false),
            ("crates-host/ports/src/can.rs", false),
            ("crates-host/adapters/src/can.rs", false),
        ] {
            assert_eq!(kept(path), wanted, "{protocol}: {path}");
        }
    }
}

const MASTER_FILE: &str = "crates-host/domain/src/modbus/master.rs";

/// Master Modbus выключен по умолчанию: ответ «false», а файл логики master'а
/// хук удаляет — вместе с ним остальное вырезает Liquid.
#[test]
fn modbus_master_is_off_by_default() {
    let result = run_cascade_with(
        &compile_script(),
        "g474re",
        &[("protocol", "modbus"), ("interface", "uart")],
    )
    .expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("modbus_master").map(String::as_str),
        Some("false")
    );
    assert!(
        result.deleted.iter().any(|path| path == MASTER_FILE),
        "master.rs без modbus_master остался: {:?}",
        result.deleted
    );
}

/// `modbus_master=yes` (и старое `true`) при Modbus оставляет файл master'а.
#[test]
fn modbus_master_yes_keeps_the_master() {
    for answer in ["yes", "true"] {
        let result = run_cascade_with(
            &compile_script(),
            "g474re",
            &[
                ("protocol", "modbus"),
                ("interface", "uart"),
                ("modbus_master", answer),
            ],
        )
        .expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("modbus_master").map(String::as_str),
            Some("true"),
            "ответ {answer}"
        );
        assert!(
            !result.deleted.iter().any(|path| path == MASTER_FILE),
            "master.rs удалён при modbus_master={answer}: {:?}",
            result.deleted
        );
    }
}

/// Без Modbus master'у нечем быть: «yes» молча приводится к «false», а файл
/// не удаляется отдельно — каталог `modbus` уходит целиком.
#[test]
fn modbus_master_is_false_without_modbus() {
    for (protocol, interface) in [("none", "none"), ("protobuf", "uart"), ("canopen", "can")] {
        for preset in [None, Some("yes")] {
            let mut vars = vec![("protocol", protocol), ("interface", interface)];
            if let Some(answer) = preset {
                vars.push(("modbus_master", answer));
            }
            let result = run_cascade_with(&compile_script(), "g474re", &vars)
                .expect("каскад не должен падать");
            assert_eq!(
                result.vars.get("modbus_master").map(String::as_str),
                Some("false"),
                "{protocol}, ответ {preset:?}"
            );
            assert!(
                !result.deleted.iter().any(|path| path == MASTER_FILE),
                "{protocol}: файл удаляется второй раз: {:?}",
                result.deleted
            );
            assert!(
                result
                    .deleted
                    .iter()
                    .any(|path| path == "crates-host/domain/src/modbus"),
                "{protocol}: каталог modbus остался: {:?}",
                result.deleted
            );
        }
    }
}

/// CANopen поверх CAN: хук берёт вид контроллера из таблицы интерфейсов
/// (имя блока и рецепт в `docs/modules/bsp-can.md` у FDCAN и bxCAN разные) и
/// оставляет крейт словаря.
#[test]
fn canopen_picks_the_can_controller_of_the_chip() {
    for (chip, kind) in [("g474re", "fdcan"), ("f407ve", "bxcan")] {
        let result = run_cascade_with(
            &compile_script(),
            chip,
            &[("protocol", "canopen"), ("interface", "can")],
        )
        .expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("can_kind").map(String::as_str),
            Some(kind),
            "{chip}"
        );
        for kept in [
            "crates-cross/bsp/src/can.rs",
            "crates-host/canopen",
            "crates-host/domain/src/canopen.rs",
            "crates-host/domain/src/canopen",
        ] {
            assert!(
                !result.deleted.iter().any(|path| path == kept),
                "{chip}: {kept} удалён"
            );
        }
    }
}

/// Блок интерфейса берётся из таблицы имён чипа, а не по соглашению: у
/// STM32C011 нет `USART2`, у STM32H723 нет `USB_OTG_FS`.
#[test]
fn the_interface_block_is_named_by_the_chip() {
    for (chip, protocol, iface, block) in [
        ("c011d6", "modbus", "uart", "USART1"),
        ("g474re", "canopen", "can", "FDCAN1"),
        ("f407ve", "canopen", "can", "CAN1"),
        ("h723ve", "modbus", "usb", "USB_OTG_HS"),
        ("f407ve", "modbus", "usb", "USB_OTG_FS"),
        ("f407ve", "modbus", "tcp", "ETH"),
    ] {
        // Без OTA: у малого чипа (C011, 32 KiB) иначе граф не помещается в
        // раздел приложения, и хук отвергает протокол раньше, чем дойдёт до имени.
        let result = run_cascade_with(
            &compile_script(),
            chip,
            &[("protocol", protocol), ("interface", iface), ("ota", "no")],
        )
        .expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("iface_periph").map(String::as_str),
            Some(block),
            "{chip} {iface}"
        );
    }
}

/// Протокол по сети оставляет стек — `bsp::net`, `adapters::net`, узел
/// `domain::net` — только у чипа с выведенным Ethernet.
#[test]
fn protocol_over_network_keeps_the_net_stack() {
    for iface in ["tcp", "udp"] {
        let result = run_cascade_with(
            &compile_script(),
            "h743zg",
            &[("protocol", "modbus"), ("interface", iface)],
        )
        .expect("каскад не должен падать");
        for kept in [
            "crates-cross/bsp/src/net.rs",
            "crates-host/adapters/src/net.rs",
            "crates-host/adapters/src/net",
            "crates-host/domain/src/net.rs",
            "crates-host/domain/src/modbus.rs",
            "crates-host/ports/src/modbus",
        ] {
            assert!(
                !result.deleted.iter().any(|path| path == kept),
                "{iface}: {kept} удалён"
            );
        }
        assert!(
            result
                .deleted
                .iter()
                .any(|path| path == "crates-host/adapters/src/serial.rs"),
            "{iface}: заглушка UART осталась"
        );
    }
}

/// Протокол по USB (protobuf и Modbus RTU): хук берёт вид USB-блока из таблицы интерфейсов (у G4 —
/// USB device, у F4 — OTG) и оставляет устройство `embassy-usb`.
#[test]
fn protocol_over_usb_picks_the_usb_block_of_the_chip() {
    for (chip, kind, protocol) in [("g474re", "usb", "protobuf"), ("f407ve", "otg", "modbus")] {
        let result = run_cascade_with(
            &compile_script(),
            chip,
            &[("protocol", protocol), ("interface", "usb")],
        )
        .expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("usb_kind").map(String::as_str),
            Some(kind),
            "{chip}"
        );
        for kept in [
            "crates-cross/bsp/src/usb.rs",
            "crates-host/domain/src/usb.rs",
        ] {
            assert!(
                !result.deleted.iter().any(|path| path == kept),
                "{chip}: {kept} удалён"
            );
        }
    }
}

/// Сочетания, при которых узел протокола не встал бы в граф или обслуживать
/// интерфейс было бы некому, генерацию останавливают — с объяснением, а не
/// проектом, который молча делает не то.
#[test]
fn impossible_protocol_answers_are_rejected() {
    /// Чип, ответы, часть ожидаемого сообщения об ошибке.
    type Rejected<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let script = compile_script();
    let cases: [Rejected; 9] = [
        // У G071 USB нет.
        (
            "g071rb",
            &[("protocol", "protobuf"), ("interface", "usb")],
            "нет выведенного блока USB, OTG",
        ),
        // У G474 нет Ethernet.
        (
            "g474re",
            &[("protocol", "modbus"), ("interface", "tcp")],
            "нет выведенного блока ETH",
        ),
        // CANopen — только поверх CAN.
        (
            "h743zg",
            &[("protocol", "canopen"), ("interface", "udp")],
            "работает поверх can, а не",
        ),
        // У G071 нет CAN-контроллера вовсе.
        (
            "g071rb",
            &[("protocol", "canopen"), ("interface", "can")],
            "нет выведенного блока FDCAN, BXCAN",
        ),
        (
            "f407ve",
            &[("protocol", "modbus")],
            "работает поверх uart, tcp, udp, usb, а не",
        ),
        ("f407ve", &[("interface", "uart")], "без протокола"),
        (
            "f407ve",
            &[("protocol", "opcua"), ("interface", "uart")],
            "protocol: ожидается canopen, modbus, none, protobuf, а не",
        ),
        (
            "f407ve",
            &[
                ("protocol", "protobuf"),
                ("interface", "uart"),
                ("graph", "no"),
            ],
            "нужен граф",
        ),
        // Граф здесь снимает размер раздела, а не ответ.
        (
            "l011f4",
            &[("protocol", "modbus"), ("interface", "uart")],
            "нужен граф",
        ),
    ];
    for (chip, preset, reason) in cases {
        let err = run_cascade_with(&script, chip, preset)
            .err()
            .unwrap_or_else(|| panic!("{chip} {preset:?}: каскад не остановился"));
        assert!(
            err.to_string().contains(reason),
            "{chip} {preset:?}: ждали «{reason}», а получили: {err}"
        );
    }
}

/// Ответ «нет» на вопрос про граф окончателен и на просторном чипе.
#[test]
fn declining_the_graph_is_final() {
    let result = run_cascade_with(&compile_script(), "f407ve", &[("graph", "no")])
        .expect("каскад не должен падать");

    assert_eq!(result.vars.get("graph").map(String::as_str), Some("false"));
}

/// Ветка ручной раскладки (`--define write_size=...`) тоже обязана раздать
/// `graph`.
///
/// Тест не теоретический: ровно здесь ответ и терялся — `variable::set` стоял
/// в соседней ветке, и пользователь, ответивший «да», молча получал проект без
/// графа (Liquid сравнивает с `"true"`, а сырое `"yes"` из `[placeholders]` ему
/// не равно).
#[test]
fn manual_layout_still_answers_about_the_graph() {
    let script = compile_script();

    let kept = run_cascade_with(&script, "f407ve", &[("write_size", "8"), ("graph", "yes")])
        .expect("каскад не должен падать");
    assert_eq!(kept.vars.get("graph").map(String::as_str), Some("true"));

    let dropped = run_cascade_with(&script, "f407ve", &[("write_size", "8"), ("graph", "no")])
        .expect("каскад не должен падать");
    assert_eq!(dropped.vars.get("graph").map(String::as_str), Some("false"));
}

/// Релиз по тегу выкладывает `app.bin`, а тот собирается только там, где есть
/// раздел `DFU`. Тело workflow и так под Liquid-условием — снос файла нужен
/// ради другого: пустой файл в `.github/workflows` GitHub всё равно показал бы
/// в списке рабочих процессов.
///
/// Проверяются обе ветки отказа от OTA: ответ пользователя и чип, куда схема
/// не помещается, — код удаления в них разный.
#[test]
fn the_release_workflow_goes_away_together_with_ota() {
    const RELEASE: &str = ".github/workflows/release.yml";
    let ast = compile_script();

    let declined = run_cascade_with(&ast, "f407ve", &[("ota", "no")]).expect("каскад с ota=no");
    assert!(
        declined.deleted.contains(&RELEASE.to_string()),
        "при отказе от OTA релизный workflow должен быть удалён, а удалено: {:?}",
        declined.deleted
    );

    // h723ve схему не вмещает — OTA отключается независимо от ответа.
    let no_room =
        run_cascade_with(&ast, "h723ve", &[("ota", "internal")]).expect("каскад без места");
    assert!(
        no_room.deleted.contains(&RELEASE.to_string()),
        "на чипе без места для OTA релизный workflow должен быть удалён, а удалено: {:?}",
        no_room.deleted
    );

    // А там, где OTA есть, файл обязан остаться: удали его — и релиза не
    // случится вовсе, причём молча.
    let with_ota = run_cascade_to(&ast, "f407ve").expect("каскад с OTA");
    assert!(
        !with_ota.deleted.contains(&RELEASE.to_string()),
        "с OTA релизный workflow удалять нельзя, а удалено: {:?}",
        with_ota.deleted
    );
}

/// OTA не предлагается там, где в раздел под приложение не влезает даже пустая
/// прошивка.
///
/// Точки замерены сборкой, а не выбраны на глаз: `f030c8` (15 KiB под
/// приложение) переполнял раздел на 848 байт, `l051c8` (16256) — на 272,
/// `c051c8` (14 KiB) — на 1904. Такой проект генерировался, но собрать его
/// было нельзя вовсе, хотя без OTA-схемы прошивка занимает пятую часть того же
/// чипа.
#[test]
fn ota_is_dropped_where_the_app_partition_is_too_small() {
    let ast = compile_script();

    // f030r8 и f051r8 здесь не для полноты: это ходовые Nucleo, на которых
    // потерю OTA заметят первыми — пусть отказ будет закреплён именно на них.
    for suffix in ["f030c8", "l051c8", "c051c8", "f030r8", "f051r8"] {
        let result = run_cascade_to(&ast, suffix)
            .unwrap_or_else(|err| panic!("каскад до {suffix} упал: {err}"));

        assert_eq!(
            result.vars.get("ota").map(String::as_str),
            Some("false"),
            "{suffix}: под приложение остаётся меньше минимальной прошивки, OTA предлагать нельзя"
        );
        assert!(
            result.deleted.contains(&"crates-cross/boot".to_string()),
            "{suffix}: bootloader должен быть удалён, а удалено: {:?}",
            result.deleted
        );

        // Приложению достаётся весь flash — то, ради чего OTA и снимается.
        let app = written(&result, "crates-cross/app/memory.x");
        assert!(
            app.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 64K"),
            "{suffix}: приложение должно линковаться с базы flash:\n{app}"
        );
    }
}

/// Обратная сторона того же порога: чип, где места достаточно, OTA сохраняет.
///
/// Без этой половины предыдущий тест остался бы зелёным, даже если бы порог
/// снимал OTA у всех подряд.
#[test]
fn ota_survives_where_the_app_partition_is_large_enough() {
    let result = run_cascade_to(&compile_script(), "l073rz").expect("каскад до l073rz");

    assert_eq!(result.vars.get("ota").map(String::as_str), Some("true"));
    assert!(
        !result.deleted.contains(&"crates-cross/boot".to_string()),
        "bootloader удалять не за что: под приложение отведено 80 KiB"
    );
}

/// Раздела настроек во флеше нет там, где флеш стирается в ноль.
///
/// `sequential-storage` считает стёртым состоянием `0xFF` — и заголовок записи
/// ищет как «все байты `0xFF`», и состояние страницы определяет по нулевым
/// маркерам. На L0/L1 свежестёртая страница выглядит «уже закрытой», то есть
/// хранилище во флеше молча не работает. Тот же класс дефекта, что и с
/// `embassy-boot` (см. `erase_zero` в chip-select.rhai), только у второй
/// библиотеки.
///
/// У L0/L1 есть внутренняя EEPROM — настройки уходят туда (обёртка
/// `adapters::eeprom` инвертирует байты, и заводской ноль читается как `0xFF`),
/// а раздела `CONFIG` во флеше нет. У WB10CC/WB15CC флеш тоже стирается в
/// ноль, а EEPROM нет — им настройки по-прежнему не предлагаются.
#[test]
fn settings_avoid_flash_that_erases_to_zero() {
    let ast = compile_script();

    for suffix in ["l073rz", "l051c8"] {
        let result = run_cascade_with(&ast, suffix, &[("config", "internal")])
            .unwrap_or_else(|err| panic!("каскад до {suffix} упал: {err}"));

        assert_eq!(
            result.vars.get("erase_zero").map(String::as_str),
            Some("true"),
            "{suffix}: чип из семейства со стиранием в ноль"
        );
        assert_eq!(
            result.vars.get("config_store").map(String::as_str),
            Some("eeprom"),
            "{suffix}: настройки — во внутренней EEPROM"
        );
        let app = written(&result, "crates-cross/app/memory.x");
        assert!(
            region_origin(&app, "CONFIG").is_none(),
            "{suffix}: раздела настроек во флеше быть не должно:\n{app}"
        );
    }

    // У L010F4 EEPROM есть, но 128 байт — меньше двух страниц хранилища.
    for suffix in ["wb10cc", "wb15cc", "l010f4"] {
        let result = run_cascade_with(&ast, suffix, &[("config", "internal")])
            .unwrap_or_else(|err| panic!("каскад до {suffix} упал: {err}"));

        assert_eq!(
            result.vars.get("config").map(String::as_str),
            Some("false"),
            "{suffix}: флеш стирается в ноль, EEPROM нет или она мала — настройкам негде жить"
        );
        assert!(
            result
                .deleted
                .contains(&"crates-cross/bsp/src/config.rs".to_string()),
            "{suffix}: модуль настроек должен быть удалён, а удалено: {:?}",
            result.deleted
        );
    }
}

/// EEPROM раскладка flash не нужна, поэтому и при ручной раскладке
/// (`--define write_size=...`) настройки на L0/L1 остаются — в EEPROM.
#[test]
fn eeprom_settings_survive_a_manual_layout() {
    let result = run_cascade_with(
        &compile_script(),
        "l073rz",
        &[("config", "internal"), ("write_size", "4")],
    )
    .expect("каскад до l073rz");

    assert_eq!(result.vars.get("config").map(String::as_str), Some("true"));
    assert_eq!(
        result.vars.get("config_store").map(String::as_str),
        Some("eeprom")
    );
    assert_eq!(
        result.vars.get("shared_flash").map(String::as_str),
        Some("true"),
        "EEPROM пишется через общий Flash"
    );
}

/// `config=external` — слот под драйвер внешней флеши: внутренний флеш под
/// настройки не тратится, раздела `CONFIG` нет, общий `Flash` без OTA не
/// создаётся вовсе.
#[test]
fn external_settings_take_no_internal_flash() {
    let ast = compile_script();
    let result = run_cascade_with(&ast, "h743zi", &[("config", "external"), ("ota", "no")])
        .expect("каскад до h743zi");

    assert_eq!(result.vars.get("config").map(String::as_str), Some("true"));
    assert_eq!(
        result.vars.get("config_store").map(String::as_str),
        Some("external")
    );
    assert_eq!(
        result.vars.get("shared_flash").map(String::as_str),
        Some("false")
    );
    let app = written(&result, "crates-cross/app/memory.x");
    assert!(region_origin(&app, "CONFIG").is_none(), "{app}");
    assert!(
        app.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 2048K"),
        "весь флеш — приложению:\n{app}"
    );
}

/// Обратная сторона: там, где стирание обычное, раздел настроек по-прежнему
/// выделяется. Без этого предыдущий тест был бы зелёным и при `config`,
/// снятом у всех подряд.
#[test]
fn the_settings_partition_survives_on_ordinary_flash() {
    let result = run_cascade_with(&compile_script(), "g071rb", &[("config", "internal")])
        .expect("каскад до g071rb");

    assert_eq!(
        result.vars.get("erase_zero").map(String::as_str),
        Some("false")
    );
    assert_eq!(result.vars.get("config").map(String::as_str), Some("true"));
}

/// С OTA на неравных секторах раздел настроек ложится в мелкие сектора перед
/// ACTIVE (у F407 — 2–3 по 16 KiB), и OTA при этом остаётся; без OTA — в хвост
/// по странице чипа. Раньше на F407 раздел в хвосте (2×128 KiB) вытеснял OTA.
#[test]
fn settings_go_to_small_sectors_with_ota_and_to_the_tail_without() {
    let ast = compile_script();

    let with_ota =
        run_cascade_with(&ast, "f407ve", &[("config", "internal")]).expect("каскад до f407ve");
    assert_eq!(with_ota.vars.get("ota").map(String::as_str), Some("true"));
    assert_eq!(
        with_ota.vars.get("config_erase_size").map(String::as_str),
        Some("16_384")
    );
    assert_eq!(
        with_ota.vars.get("state_erase_size").map(String::as_str),
        Some("65_536")
    );
    let app = written(&with_ota, "crates-cross/app/memory.x");
    assert!(
        app.contains("CONFIG            (rx)  : ORIGIN = 0x08008000, LENGTH = 32K"),
        "{app}"
    );
    let boot = written(&with_ota, "crates-cross/boot/memory.x");
    assert!(
        boot.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 32K"),
        "{boot}"
    );

    let without = run_cascade_with(&ast, "f407ve", &[("config", "internal"), ("ota", "no")])
        .expect("каскад до f407ve");
    assert_eq!(
        without.vars.get("config_erase_size").map(String::as_str),
        Some("131_072")
    );
    let app = written(&without, "crates-cross/app/memory.x");
    assert!(
        app.contains("CONFIG            (rx)  : ORIGIN = 0x08040000, LENGTH = 256K"),
        "{app}"
    );
}

/// Значение `watchdog_peripheral`, которое хук выбирает для чипа. У двухъядерных
/// H745 у каждого ядра свой сторож (`IWDG1` у CM7, `IWDG2` у CM4) — перепутанный
/// блок собрался бы, но сторож не кормился бы никогда.
#[test]
fn watchdog_peripheral_follows_the_chip_metadata() {
    let ast = compile_script();
    for (suffix, expected) in [
        ("h745zi-cm4", "IWDG2"),
        ("h745zi-cm7", "IWDG1"),
        ("h723ve", "IWDG1"),
        ("f407ve", "IWDG"),
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("watchdog_peripheral").map(String::as_str),
            Some(expected),
            "{suffix}"
        );
    }
}

/// Банк 2 за разрывом (H743ZG: 2×512 KiB, банк 2 с 0x0810_0000): DFU в нём, а
/// его символы — от начала банка 2, потому что прошивка пишет его через
/// `Bank2Region` (`dfu_bank = true`); цельный `Flash` туда не дотягивается.
#[test]
fn dfu_behind_a_bank_gap_is_addressed_from_the_bank() {
    let result = run_cascade_to(&compile_script(), "h743zg").expect("каскад не должен падать");
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("true"));
    assert_eq!(
        result.vars.get("dfu_bank").map(String::as_str),
        Some("true")
    );
    for file in ["crates-cross/boot/memory.x", "crates-cross/app/memory.x"] {
        let memory = written(&result, file);
        assert_eq!(
            region_origin(&memory, "DFU").as_deref(),
            Some("0x08100000"),
            "{file}"
        );
        assert!(
            memory.contains("__bootloader_dfu_start = ORIGIN(DFU) - 0x08100000;"),
            "{file}: {memory}"
        );
        // Всё остальное — по-прежнему от базы flash.
        assert!(
            memory.contains("__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - 0x08000000;"),
            "{file}: {memory}"
        );
    }
}

/// Внешняя OTA на чипе с разрывом — как на любом другом: DFU на внешней
/// флеши, и регион банка 2 прошивке не нужен (`dfu_bank = false`).
#[test]
fn external_ota_on_a_bank_gap_chip_leaves_bank_2_alone() {
    let result = run_cascade_with(&compile_script(), "h743zg", &[("ota", "external")])
        .expect("каскад не должен падать");
    assert_eq!(
        result.vars.get("ota_store").map(String::as_str),
        Some("external")
    );
    assert_eq!(
        result.vars.get("dfu_bank").map(String::as_str),
        Some("false")
    );
}

/// Переключение в одобанковый режим — у тех, кто с завода двухбанковый и получил
/// одобанковую раскладку, с вариантом регистров своего семейства. F4 с 1 MiB и F7 с
/// завода одобанковые — переключать нечего; у чипа с одной картой памяти выбора нет
/// вовсе. Без переключения `banks.rs` удаляется.
#[test]
fn bank_switch_follows_the_factory_bank_mode() {
    let ast = compile_script();
    for (suffix, expected, bit) in [
        ("g474re", "g4", "22"),
        ("g0b1rc", "g0", "21"),
        // L4R: у 2 MiB (наибольший объём серии) бит DBANK, у 1 MiB — DB1M.
        ("l4r5zi", "l4", "22"),
        ("l4r5zg", "l4", "21"),
        ("l552ze", "l5", "22"),
        ("f429zg", "", ""),
        ("f767zi", "", ""),
        ("h743zg", "", ""),
        ("f407ve", "", ""),
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("bank_switch").map(String::as_str),
            Some(expected),
            "{suffix}"
        );
        assert_eq!(
            result.vars.get("dual_bank_bit").map(String::as_str),
            Some(bit),
            "{suffix}: бит"
        );
        assert_eq!(
            result
                .deleted
                .iter()
                .any(|path| path == "crates-cross/bsp/src/banks.rs"),
            expected.is_empty(),
            "{suffix}: banks.rs"
        );
    }
}

/// RNG — из таблицы `chip-data/rng.rhai`: прерывание и клок по линии. Чипу
/// без RNG модуль `bsp::rng` и его рецепт удаляются.
#[test]
fn rng_follows_the_chip_table() {
    let ast = compile_script();
    for (suffix, irq, clock, source) in [
        ("f429zg", "HASH_RNG", "pll48", ""),
        ("g081rb", "AES_RNG", "rngsel", "SYS"),
        ("l073rz", "RNG_LPUART1", "clk48sel", "HSI48"),
        ("h743zg", "RNG", "default", ""),
        ("g071rb", "", "", ""),
        // HSI48 без CRS `embassy-stm32` не включает — RNG нечем тактировать.
        ("wb15cc", "", "", ""),
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        for (var, expected) in [
            ("rng_irq", irq),
            ("rng_clock", clock),
            ("rng_source", source),
        ] {
            assert_eq!(
                result.vars.get(var).map(String::as_str),
                Some(expected),
                "{suffix}: {var}"
            );
        }
    }
}

/// Файлы `bsp::rng` остаются только под tcp/udp (сид стека `embassy-net`) и
/// только у чипа с RNG; без интерфейса или на другом интерфейсе — удалены.
#[test]
fn rng_files_stay_only_for_network_interfaces() {
    let ast = compile_script();
    let files = ["crates-cross/bsp/src/rng.rs", "docs/modules/bsp-rng.md"];
    let deleted = |suffix: &str, preset: &[(&str, &str)]| {
        let result = run_cascade_with(&ast, suffix, preset).expect("каскад не должен падать");
        files
            .iter()
            .map(|file| result.deleted.iter().any(|path| path == file))
            .collect::<Vec<_>>()
    };
    // (чип, ответы, удалены ли файлы)
    type RngCase<'a> = (&'a str, &'a [(&'a str, &'a str)], bool);
    let cases: [RngCase; 6] = [
        ("h743zg", &[], true),
        (
            "h743zg",
            &[("protocol", "modbus"), ("interface", "tcp")],
            false,
        ),
        (
            "h743zg",
            &[("protocol", "protobuf"), ("interface", "udp")],
            false,
        ),
        (
            "h743zg",
            &[("protocol", "modbus"), ("interface", "uart")],
            true,
        ),
        (
            "f407ve",
            &[("protocol", "modbus"), ("interface", "usb")],
            true,
        ),
        (
            "f429zg",
            &[("protocol", "modbus"), ("interface", "udp")],
            false,
        ),
    ];
    for (suffix, preset, gone) in cases {
        assert_eq!(
            deleted(suffix, preset),
            vec![gone; 2],
            "{suffix} {preset:?}"
        );
    }
}

/// `CPU_FREQ_BOOST` — только у линии RM0468 (H723/725/730/733/735): им bsp
/// получает `clocks.rs`, остальным файл удаляется. Соседняя H743 — та же
/// серия, но boost у неё нет, и `clocks.rs` с регистрами `OPTSR2` там писал бы
/// не туда.
#[test]
fn cpu_freq_boost_only_on_the_rm0468_line() {
    let ast = compile_script();
    for (suffix, expected) in [
        ("h723ve", true),
        ("h735ig", true),
        ("h743zi", false),
        ("f407ve", false),
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("cpu_freq_boost").map(String::as_str),
            Some(if expected { "true" } else { "false" }),
            "{suffix}"
        );
        assert_eq!(
            !result
                .deleted
                .iter()
                .any(|path| path == "crates-cross/bsp/src/clocks.rs"),
            expected,
            "{suffix}: clocks.rs"
        );
    }
}

/// Ключи первого уровня таблицы `export const NAME = #{ ... };` из файла в
/// `chip-data/`: строки с отступом ровно в четыре пробела, начинающиеся с
/// `"ключ":`.
fn table_keys(script: &str, name: &str) -> Vec<String> {
    let header = format!("export const {name} = #{{");
    let begin = script
        .find(&header)
        .unwrap_or_else(|| panic!("{name} не найден"));
    let body = &script[begin + header.len()..];
    let end = body
        .find("\n};")
        .unwrap_or_else(|| panic!("конец {name} не найден"));
    body[..end]
        .lines()
        .filter_map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let rest = line.strip_prefix("    \"")?;
            Some(rest.split_once('"')?.0.to_string())
        })
        .collect()
}

/// Таблицы, ключом которых служит суффикс чипа, не должны знать чипов, которых
/// нет в `CHIPS`: ключ-опечатка (или чип, ушедший из `embassy-stm32`) молча
/// никогда бы не сработал. Обратное неверно и не проверяется — таблицы
/// покрывают лишь часть чипов.
#[test]
fn table_keys_are_a_subset_of_chips() {
    let data = repo_root().join("chip-data");
    let read = |path: PathBuf| {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("не удалось прочитать {}: {err}", path.display()))
    };
    let layouts: Vec<String> = std::fs::read_dir(data.join("layout"))
        .expect("не удалось прочитать chip-data/layout")
        .map(|entry| entry.expect("элемент каталога").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rhai"))
        .map(read)
        .collect();
    let chips: std::collections::HashSet<String> = all_chip_suffixes().into_iter().collect();
    // Легитимных исключений нет: если появятся, перечислить здесь явно, с причиной.
    let exceptions: &[(&str, &str)] = &[];
    for (table, files) in [
        ("MEMORY_LAYOUT", layouts),
        ("BANK_MODE", vec![read(data.join("bank_mode.rhai"))]),
        ("WATCHDOG", vec![read(data.join("watchdog.rhai"))]),
        ("PACKAGE_CHOICES", vec![read(data.join("packages.rhai"))]),
        ("INTERFACES", vec![read(data.join("interfaces.rhai"))]),
    ] {
        let keys: Vec<String> = files
            .iter()
            .flat_map(|script| table_keys(script, table))
            .collect();
        assert!(
            keys.len() >= 20,
            "{table}: подозрительно мало ключей: {}",
            keys.len()
        );
        let unknown: Vec<&String> = keys
            .iter()
            .filter(|key| {
                !chips.contains(*key)
                    && !exceptions
                        .iter()
                        .any(|(t, k)| *t == table && *k == key.as_str())
            })
            .collect();
        assert!(
            unknown.is_empty(),
            "{table}: ключи, которых нет в CHIPS: {unknown:?}"
        );
    }
}

/// `INTERFACES`, в отличие от остальных таблиц, покрывает каждый чип: по ней
/// хук проверяет ответ об интерфейсе, и чип без записи отвергался бы с любым
/// ответом, кроме «без интерфейса».
#[test]
fn every_chip_has_interfaces() {
    let script = std::fs::read_to_string(repo_root().join("chip-data/interfaces.rhai"))
        .expect("не удалось прочитать chip-data/interfaces.rhai");
    let keys: std::collections::HashSet<String> =
        table_keys(&script, "INTERFACES").into_iter().collect();
    let missing: Vec<String> = all_chip_suffixes()
        .into_iter()
        .filter(|chip| !keys.contains(chip))
        .collect();
    assert!(missing.is_empty(), "INTERFACES: нет записи для {missing:?}");
}

/// Страница обмена (`page_size`) и признак `swap_paged`: расширенная страница
/// есть только у внутренней OTA на чипе с равными секторами, и тогда
/// `page_size` — именно она. L073RZ (страница 128 байт) расширяется, F407VE
/// (неравные сектора), H743ZG (DFU в банке 2) — нет; без OTA, с внешней OTA и
/// в ручной раскладке `swap_paged` = "false".
#[test]
fn swap_page_follows_the_layout() {
    let ast = compile_script();
    let var = |result: &CascadeResult, name: &str| result.vars.get(name).cloned();

    let wide = run_cascade_to(&ast, "l073rz").expect("каскад не должен падать");
    assert_eq!(var(&wide, "ota").as_deref(), Some("true"));
    assert_eq!(var(&wide, "swap_paged").as_deref(), Some("true"));
    let page: u64 = var(&wide, "page_size")
        .and_then(|v| v.parse().ok())
        .expect("page_size — число");
    assert!(page > 128 && page <= 8192, "страница обмена {page}");
    let boot = written(&wide, "crates-cross/boot/memory.x");
    // ACTIVE и DFU кратны странице обмена, DFU не меньше чем на страницу длиннее.
    let (active, dfu) = (
        region_length(&boot, "ACTIVE").expect("ACTIVE"),
        region_length(&written(&wide, "crates-cross/app/memory.x"), "DFU").expect("DFU"),
    );
    assert!(active.is_multiple_of(page), "{boot}");
    assert!(dfu >= active + page && dfu.is_multiple_of(page), "{boot}");
    // STATE — по-прежнему в физических секторах (128 байт).
    assert_eq!(var(&wide, "state_erase_size").as_deref(), Some("128"));
    assert!(
        region_length(&boot, "FLASH").expect("FLASH") >= 26 * 1024,
        "{boot}"
    );

    for (chip, preset) in [
        ("f407ve", &[][..]),
        ("h743zg", &[][..]),
        ("l073rz", &[("ota", "no")][..]),
        ("l073rz", &[("ota", "external")][..]),
        ("l073rz", &[("write_size", "4")][..]),
    ] {
        let result = run_cascade_with(&ast, chip, preset).expect("каскад не должен падать");
        assert_eq!(
            var(&result, "swap_paged").as_deref(),
            Some("false"),
            "{chip} {preset:?}"
        );
    }
    let f4 = run_cascade_to(&ast, "f407ve").expect("каскад не должен падать");
    assert_eq!(var(&f4, "page_size").as_deref(), Some("131072"));
    let ext = run_cascade_with(&ast, "l073rz", &[("ota", "external")]).expect("каскад");
    assert_eq!(var(&ext, "page_size").as_deref(), Some(""));
}

/// У каждого чипа с OTA — с настройками и без — зона bootloader'а не меньше 26
/// KiB (подписанный `boot` ≈23 KiB), страница обмена делит `ACTIVE`/`DFU` и
/// кратна физической, а `swap_paged` ровно тогда, когда она крупнее.
#[test]
fn bootloader_zone_and_swap_page_hold_for_every_chip() {
    let ast = compile_script();
    let mut failures = Vec::new();
    let mut paged = 0;
    for suffix in all_chip_suffixes() {
        for preset in [&[][..], &[("config", "internal")][..]] {
            let Ok(result) = run_cascade_with(&ast, &suffix, preset) else {
                continue; // падения каскада ловят соседние тесты
            };
            if result.vars.get("ota").map(String::as_str) != Some("true") {
                continue;
            }
            let boot = written(&result, "crates-cross/boot/memory.x");
            let app = written(&result, "crates-cross/app/memory.x");
            let zone = region_length(&boot, "FLASH").unwrap_or(0);
            let page: u64 = result
                .vars
                .get("page_size")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let (active, dfu) = (
                region_length(&boot, "ACTIVE").unwrap_or(1),
                region_length(&app, "DFU").unwrap_or(0),
            );
            let swap_paged = result.vars.get("swap_paged").map(String::as_str) == Some("true");
            let mut fail = |what: String| failures.push(format!("{suffix} {preset:?}: {what}"));
            if zone < 26 * 1024 {
                fail(format!("зона bootloader'а {zone} < 26 KiB"));
            }
            if page == 0
                || !active.is_multiple_of(page)
                || !dfu.is_multiple_of(page)
                || dfu < active + page
            {
                fail(format!(
                    "ACTIVE {active} / DFU {dfu} не по странице обмена {page}"
                ));
            }
            if swap_paged {
                paged += 1;
                if page > 8192 {
                    fail(format!("страница обмена {page} крупнее SWAP_PAGE_MAX"));
                }
                if result.vars.get("dfu_bank").map(String::as_str) == Some("true") {
                    fail("расширенная страница при DFU в банке 2".to_string());
                }
            }
        }
    }
    assert!(
        paged > 300,
        "подозрительно мало расширенных страниц: {paged}"
    );
    assert!(
        failures.is_empty(),
        "нарушено у {} раскладок:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
