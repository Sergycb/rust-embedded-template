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
/// Показанные меню: заголовок и варианты (см. `CascadeResult::prompts`).
type Prompts = Rc<RefCell<Vec<(String, Vec<String>)>>>;

/// Итог одного прогона: переменные (chip/chip_feature/cpu/target/write_size/
/// ota/bank_mode), файлы, записанные через `file::write` (memory.x), построчно,
/// и пути, снесённые через `file::delete` (crates-cross/boot у чипов без OTA).
struct CascadeResult {
    vars: HashMap<String, String>,
    files: HashMap<String, Vec<String>>,
    deleted: Vec<String>,
    prints: Vec<String>,
    /// Показанные меню: заголовок и варианты в том порядке, в каком их видит
    /// пользователь. Каскад по символам (выбор чипа) сюда не попадает.
    prompts: Vec<(String, Vec<String>)>,
}

/// Прогоняет уже скомпилированный chip-select.rhai один раз, ведя каскад к
/// `target_suffix` (суффикс без "stm32", например "f407ve" или "l151c6-a").
fn run_cascade_to(
    script: &Script,
    target_suffix: &str,
) -> Result<CascadeResult, Box<EvalAltResult>> {
    run_cascade_with(script, target_suffix, &[])
}

/// Чип задан напрямую (`--define chip_feature=...`): каскад по символам и
/// отсечение чипов ответами обходятся. Так проверяется, что хук делает с
/// чипом, которому ответы не подходят, — в каскаде такой чип не показывается.
fn run_chip_with(
    script: &Script,
    suffix: &str,
    preset: &[(&str, &str)],
) -> Result<CascadeResult, Box<EvalAltResult>> {
    let chip = format!("stm32{suffix}");
    let mut all = preset.to_vec();
    all.push(("chip_feature", chip.as_str()));
    run_cascade_with(script, suffix, &all)
}

/// То же, но с заранее выставленными переменными — так проверяются ветки,
/// которые в реальной генерации приходят из `[placeholders]` или `--define`
/// (`ota`, `write_size`, `chip`).
fn run_cascade_with(
    script: &Script,
    target_suffix: &str,
    preset: &[(&str, &str)],
) -> Result<CascadeResult, Box<EvalAltResult>> {
    run_interview(script, target_suffix, preset, &[])
}

/// Прогон с ответами «пользователя»: `preset` — то, что пришло через
/// `--define`, `picks` — выбор в меню (`("ota", "no")` отвечает на вопрос
/// `ota: ...` вариантом, начинающимся со слова `no`; у пункта списка с
/// отметкой слово ищется после `[x] `/`[ ] `). Ответы одного id расходуются по
/// порядку: `[("transports", "uart"), ("transports", "готово")]` отмечает
/// пункт в первом показе меню `transports:` и закрывает его во втором. Меню без ответа (или когда
/// ответы его id кончились) ведёт себя как запуск без терминала: `variable::prompt` отвечает ошибкой «not a
/// terminal», и хук берёт умолчание.
fn run_interview(
    script: &Script,
    target_suffix: &str,
    preset: &[(&str, &str)],
    picks: &[(&str, &str)],
) -> Result<CascadeResult, Box<EvalAltResult>> {
    let prompts: Prompts = Rc::new(RefCell::new(Vec::new()));
    let picks: Vec<(String, String)> = picks
        .iter()
        .map(|(id, value)| ((*id).to_string(), (*value).to_string()))
        .collect();
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
        // Ввод строки по regex (`ssh_key:`): ответ — значение из `picks` как
        // есть, без него — «not a terminal». Regex не проверяется: его держит
        // cargo-generate, а хук проверяет ответ сам.
        let picks = picks.clone();
        let prompts = prompts.clone();
        module.set_native_fn(
            "prompt",
            move |text: &str,
                  _default: &str,
                  _regex: &str|
                  -> Result<Dynamic, Box<EvalAltResult>> {
                prompts.borrow_mut().push((text.to_string(), Vec::new()));
                let id = text.split(':').next().unwrap_or_default();
                match picks.iter().find(|(pick_id, _)| pick_id == id) {
                    Some((_, value)) => Ok(Dynamic::from(value.clone())),
                    None => Err("IO error: not a terminal".into()),
                }
            },
        );
    }
    {
        let target = target_suffix.to_string();
        let prompts = prompts.clone();
        // Сколько ответов каждого id уже израсходовано.
        let used: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
        module.set_native_fn(
            "prompt",
            move |text: &str, _default: Dynamic, choices: Array| -> Result<Dynamic, Box<EvalAltResult>> {
                if !text.starts_with("Выберите STM32") {
                    let shown: Vec<String> = choices
                        .iter()
                        .map(|c| c.clone().into_string().expect("choices — массив строк"))
                        .collect();
                    prompts.borrow_mut().push((text.to_string(), shown.clone()));
                    let id = text.split(':').next().unwrap_or_default();
                    let mut used = used.borrow_mut();
                    let nth = used.entry(id.to_string()).or_default();
                    let pick = picks.iter().filter(|(pick_id, _)| pick_id == id).nth(*nth);
                    *nth += 1;
                    return match pick {
                        Some((_, value)) => shown
                            .iter()
                            .find(|choice| {
                                let item = unmarked(choice);
                                item == value || item.starts_with(&format!("{value} "))
                            })
                            .map(|choice| Dynamic::from(choice.clone()))
                            .ok_or_else(|| {
                                format!("в меню «{id}» нет варианта «{value}»: {shown:?}").into()
                            }),
                        None => Err("IO error: not a terminal".into()),
                    };
                }
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
        prompts: prompts.borrow().clone(),
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
    let too_small = run_chip_with(
        &ast,
        "f030c8",
        &[("config", "internal"), ("ota", "internal")],
    )
    .expect("каскад не должен падать");
    let log = too_small.prints.join("\n");
    assert!(
        log.contains("Выбранное не помещается"),
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

    // H723VE: RAM — 320 KiB AXI-SRAM, самая большая область; DTCM, ITCM, SRAM
    // шины AHB (здесь — `DMA_NC`) и SRAM4 выводятся своими строками.
    let app = written(
        &run_cascade_to(&ast, "h723ve").expect("каскад не должен падать"),
        "crates-cross/app/memory.x",
    );
    assert_eq!(
        region_origin(&app, "RAM").as_deref(),
        Some("0x24000000"),
        "{app}"
    );
    for region in ["ITCM", "DTCM", "DMA_NC", "SRAM4"] {
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
        //
        // Исключение одно — секции под буферы DMA (`.dma_nc`, `.bdma_nc`): они лежат
        // в своих регионах вне `RAM`, вставлены за `.gnu.sgstubs` (за `.uninit`
        // cortex-m-rt под flip-link не собирается) и проверены сборкой.
        let dma_sections = app.matches("SECTIONS").count() == 1
            && app.contains(".dma_nc (NOLOAD)")
            && app.contains("} INSERT AFTER .gnu.sgstubs;")
            && !app.contains("> RAM");
        if app.contains("SECTIONS") && !dma_sections {
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
            let Ok(result) = run_chip_with(&ast, &suffix, preset) else {
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
fn an_ota_answer_that_does_not_fit_keeps_ota_and_leaves_memory_to_the_user() {
    // Явный ответ уважается: h723ve схему не вмещает, но bootloader остаётся, а
    // раскладку памяти пользователь заполняет сам (как при `--define write_size`).
    let result = run_chip_with(&compile_script(), "h723ve", &[("ota", "internal")])
        .expect("каскад не должен падать");
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("true"));
    assert!(!result.deleted.contains(&"crates-cross/boot".to_string()));
    assert!(result.vars.contains_key("write_size"), "ручная раскладка");
    assert!(
        result
            .prints
            .iter()
            .any(|line| line.contains("Выбранное не помещается")),
        "{:?}",
        result.prints
    );
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
    // Границы раздела уезжают числами в `[env]`, а не символами линкера.
    assert!(!app.contains("__config_start"), "{app}");
    assert_eq!(
        outside.vars.get("config_start").map(String::as_str),
        Some("655360"),
        "0x000A0000"
    );
    assert_eq!(
        outside.vars.get("config_end").map(String::as_str),
        Some("917504"),
        "0x000A0000 + 256K"
    );
}

/// Размеры разделов уезжают в `[env]` числами, и `ACTIVE_LEN` совпадает с
/// `ACTIVE` в `memory.x`: читать их через `unsafe extern` незачем.
#[test]
fn partition_bounds_travel_as_numbers() {
    let result = run_chip_with(&compile_script(), "f407ve", &[("config", "internal")])
        .expect("каскад не должен падать");
    let boot = written(&result, "crates-cross/boot/memory.x");
    let active = region_length(&boot, "ACTIVE").expect("ACTIVE в memory.x");
    assert_eq!(
        result.vars.get("active_len").map(String::as_str),
        Some(active.to_string().as_str())
    );
    let app = written(&result, "crates-cross/app/memory.x");
    let config = region_origin(&app, "CONFIG").expect("CONFIG в memory.x");
    let start: u64 = result.vars["config_start"].parse().expect("число");
    let config = u64::from_str_radix(config.trim_start_matches("0x"), 16).expect("адрес");
    assert_eq!(start, config - 0x0800_0000);
    assert!(!app.contains("__config_start"), "{app}");
}

/// Внутренняя схема не помещается, внешняя — да: меню говорит об этом прямо в
/// вариантах, а умолчание без терминала — не OTA, которая не соберётся.
#[test]
fn the_ota_menu_marks_what_does_not_fit_and_defaults_to_what_does() {
    let result = run_interview(
        &compile_script(),
        "h723ve",
        &[("chip_feature", "stm32h723ve")],
        &[],
    )
    .expect("каскад не должен падать");
    let menu = result
        .prompts
        .iter()
        .find(|(label, _)| label.starts_with("ota:"))
        .expect("вопрос про OTA не задан");
    let text = |value: &str| {
        menu.1
            .iter()
            .find(|choice| choice.starts_with(value))
            .unwrap_or_else(|| panic!("нет варианта {value}: {:?}", menu.1))
    };
    assert!(text("internal").contains("НЕ ПОМЕЩАЕТСЯ"), "{:?}", menu.1);
    assert!(!text("external").contains("НЕ ПОМЕЩАЕТСЯ"), "{:?}", menu.1);
    // Умолчание без терминала — то, что не обещает невозможного.
    assert_eq!(result.vars.get("ota").map(String::as_str), Some("false"));
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
        "crates-cross/bsp/src/stub.rs",
        "crates-host/adapters/src/fake.rs",
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
            // Кадровый канал нужен protobuf; Modbus по потоку им не пользуется.
            ("crates-host/ports/src/link.rs", protocol == "protobuf"),
            ("crates-host/canopen", false),
            ("crates-host/ports/src/can.rs", false),
            // Заглушка UART в `bsp`; драйверов-фейков USB/сети нет.
            ("crates-cross/bsp/src/stub.rs", true),
            ("crates-host/adapters/src/fake.rs", false),
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

/// Без Modbus master'у нечем быть: вопрос не задаётся, ответ «no» принимается, а
/// «yes» через `--define` — ошибка. Файл не удаляется отдельно — каталог `modbus`
/// уходит целиком.
#[test]
fn modbus_master_is_false_without_modbus() {
    let ast = compile_script();
    for (protocol, interface) in [("none", "none"), ("protobuf", "uart"), ("canopen", "can")] {
        let err = run_cascade_with(
            &ast,
            "g474re",
            &[
                ("protocol", protocol),
                ("interface", interface),
                ("modbus_master", "yes"),
            ],
        )
        .err()
        .unwrap_or_else(|| panic!("{protocol}: master без modbus принят"));
        assert!(
            err.to_string()
                .contains("master есть только у протокола modbus"),
            "{err}"
        );

        for preset in [None, Some("no")] {
            let mut vars = vec![("protocol", protocol), ("interface", interface)];
            if let Some(answer) = preset {
                vars.push(("modbus_master", answer));
            }
            let result = run_cascade_with(&ast, "g474re", &vars).expect("каскад не должен падать");
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
                .any(|path| path == "crates-cross/bsp/src/serial.rs"),
            "{iface}: модуль UART остался"
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

/// Сочетания, при которых обслуживать интерфейс было бы некому или у чипа нет
/// нужного блока, генерацию останавливают — с объяснением, а не проектом, который
/// молча делает не то.
#[test]
fn impossible_protocol_answers_are_rejected() {
    /// Чип, ответы, часть ожидаемого сообщения об ошибке.
    type Rejected<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let script = compile_script();
    let cases: [Rejected; 6] = [
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
            "нужен транспорт — задайте interface (uart, tcp, udp, usb)",
        ),
        (
            "f407ve",
            &[("protocol", "opcua"), ("interface", "uart")],
            "protocol: ожидается canopen, modbus, none, protobuf, а не",
        ),
    ];
    for (chip, preset, reason) in cases {
        let err = run_chip_with(&script, chip, preset)
            .err()
            .unwrap_or_else(|| panic!("{chip} {preset:?}: каскад не остановился"));
        assert!(
            err.to_string().contains(reason),
            "{chip} {preset:?}: ждали «{reason}», а получили: {err}"
        );
    }
}

/// Значение переменной хука.
fn var_of<'a>(result: &'a CascadeResult, name: &str) -> Option<&'a str> {
    result.vars.get(name).map(String::as_str)
}

/// Значения вариантов каждого показанного меню `id` — по порядку показа.
fn all_menu_values(result: &CascadeResult, id: &str) -> Vec<Vec<String>> {
    result
        .prompts
        .iter()
        .filter(|(label, _)| label.starts_with(&format!("{id}:")))
        .map(|(_, choices)| choices.iter().map(|c| menu_value(c).to_string()).collect())
        .collect()
}

/// Пара `interface` + `protocol` по-старому — один транспорт: список из одного
/// элемента, а прежние переменные — те же, что до списка транспортов.
#[test]
fn single_answers_keep_the_old_variables() {
    let result = run_cascade_with(
        &compile_script(),
        "c011d6",
        &[("interface", "uart"), ("protocol", "modbus"), ("ota", "no")],
    )
    .expect("каскад не должен падать");
    assert_eq!(
        var_of(&result, "links"),
        Some("uart:usart:USART1:modbus:usart1")
    );
    assert_eq!(var_of(&result, "has_uart"), Some("true"));
    assert_eq!(var_of(&result, "has_can"), Some("false"));
    assert_eq!(var_of(&result, "iface_periph"), Some("USART1"));
    assert_eq!(var_of(&result, "interface"), Some("uart"));
}

/// `--define links`: блоки назначаются по порядку списка чипа, сеть — на ETH.
#[test]
fn links_assign_blocks_in_order() {
    let result = run_cascade_with(
        &compile_script(),
        "h743zg",
        &[
            ("links", "uart:modbus,uart:modbus,tcp:modbus"),
            ("ota", "no"),
        ],
    )
    .expect("каскад не должен падать");
    assert_eq!(
        var_of(&result, "links"),
        Some(
            "uart:usart:USART1:modbus:usart1,uart:usart:USART2:modbus:usart2,tcp:eth:ETH:modbus:tcp"
        )
    );
    assert_eq!(var_of(&result, "has_net"), Some("true"));
    assert_eq!(var_of(&result, "protocol"), Some("modbus"));
}

/// Невозможный `links` останавливает генерацию с объяснением.
#[test]
fn links_reject_impossible_answers() {
    /// Чип, ответы, часть ожидаемого сообщения об ошибке.
    type Rejected<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str);
    let script = compile_script();
    let cases: [Rejected; 8] = [
        (
            "f407ve",
            &[("links", "uart"), ("interface", "uart")],
            "links: задаётся вместо interface и protocol, а не вместе с ними",
        ),
        (
            "f407ve",
            &[("links", "uart"), ("protocol", "none")],
            "links: задаётся вместо interface и protocol, а не вместе с ними",
        ),
        (
            "f407ve",
            &[("links", "uart,,tcp")],
            "links: пустой элемент в «uart,,tcp»",
        ),
        (
            "f407ve",
            &[("links", "spi")],
            "links: ожидается вид can, tcp, uart, udp, usb, а не «spi»",
        ),
        (
            "f407ve",
            &[("links", "uart:opcua")],
            "links: ожидается протокол canopen, modbus, none, protobuf, а не «opcua»",
        ),
        (
            "f407ve",
            &[("links", "can:modbus")],
            "links: протокол modbus работает поверх uart, tcp, udp, usb, а не «can»",
        ),
        (
            "f407ve",
            &[("links", "uart:modbus,tcp:protobuf")],
            "links: протокол в проекте пока один, а заданы modbus и protobuf",
        ),
        // У F042 один bxCAN.
        (
            "f042c6",
            &[("links", "can,can")],
            "блоков вида can — 1 (CAN), а запрошено 2",
        ),
    ];
    for (chip, preset, reason) in cases {
        let err = run_chip_with(&script, chip, preset)
            .err()
            .unwrap_or_else(|| panic!("{chip} {preset:?}: каскад не остановился"));
        assert!(
            err.to_string().contains(reason),
            "{chip} {preset:?}: ждали «{reason}», а получили: {err}"
        );
    }
}

/// Пробелы вокруг частей `links` допускаются, пустой элемент — нет.
#[test]
fn links_tolerate_spaces_but_not_empty_items() {
    let script = compile_script();
    let result = run_chip_with(&script, "f407ve", &[("links", "uart, tcp:modbus")])
        .expect("пробелы в links должны приниматься");
    assert_eq!(
        var_of(&result, "links"),
        Some("uart:usart:USART1:none:usart1,tcp:eth:ETH:modbus:tcp")
    );
    let err = run_chip_with(&script, "f407ve", &[("links", "uart,,tcp")])
        .err()
        .expect("пустой элемент принят");
    assert!(err.to_string().contains("links: пустой элемент"), "{err}");
}

/// В списке каждый вид — одним пунктом, без имён блоков, в порядке `uart`,
/// `can`, `ethernet`, `usb`; первый пункт — `готово`. У F407 шесть USART и
/// ETH, а пункт `uart` один, и пунктов `tcp`/`udp` нет.
#[test]
fn each_kind_is_listed_once() {
    let result = interview(&compile_script(), "f407ve", &[], &[]).expect("каскад не должен падать");
    let menus = all_menu_values(&result, "transports");
    assert_eq!(menus, [["готово", "uart", "can", "ethernet", "usb"]]);
    let shown = &result
        .prompts
        .iter()
        .find(|(label, _)| label.starts_with("transports:"))
        .expect("список не показан")
        .1;
    assert_eq!(
        shown[1],
        "[ ] uart — UART (RS-485 — тот же UART с выводом DE)"
    );
    assert!(
        shown
            .iter()
            .all(|item| !item.contains("USART") && !item.contains("ETH")),
        "{shown:?}"
    );
}

/// Когда протокол проекта выбран, следующему транспорту предлагается только он.
#[test]
fn once_a_protocol_is_chosen_only_it_and_none_are_offered() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[
            ("transports", "uart"),
            ("transports", "ethernet"),
            ("transports", "готово"),
            ("protocol", "modbus"),
        ],
    )
    .expect("каскад не должен падать");
    let menus = all_menu_values(&result, "protocol");
    assert_eq!(menus.len(), 2, "{menus:?}");
    assert_eq!(menus[1], ["none", "modbus"]);
    // Master спрашивается один раз — при первом выборе Modbus.
    assert_eq!(all_menu_values(&result, "modbus_master").len(), 1);
}

/// Вид, поверх которого протокол проекта не работает, получает транспорт без
/// протокола, и вопрос о протоколе не задаётся.
#[test]
fn a_kind_the_protocol_cannot_carry_gets_no_protocol_question() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[
            ("transports", "uart"),
            ("transports", "can"),
            ("transports", "готово"),
            ("protocol", "modbus"),
        ],
    )
    .expect("каскад не должен падать");
    assert_eq!(all_menu_values(&result, "protocol").len(), 1);
    assert_eq!(
        var_of(&result, "links"),
        Some("uart:usart:USART1:modbus:usart1,can:bxcan:CAN1:none:can1")
    );
}

/// Вопросы о протоколе идут в порядке пунктов, а не отметок: `can` отмечен
/// первым, но первым спрашивается `uart`, и после Modbus на нём у `can` вопроса
/// нет; основной транспорт — `uart`.
#[test]
fn the_protocol_menu_follows_the_marked_order() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[
            ("transports", "can"),
            ("transports", "uart"),
            ("transports", "готово"),
            ("protocol", "modbus"),
        ],
    )
    .expect("каскад не должен падать");
    let menus = all_menu_values(&result, "protocol");
    assert_eq!(menus, [["none", "modbus", "protobuf"]]);
    assert_eq!(
        var_of(&result, "links"),
        Some("uart:usart:USART1:modbus:usart1,can:bxcan:CAN1:none:can1")
    );
    assert_eq!(var_of(&result, "interface"), Some("uart"));
}

/// Транспорты идут в проект в порядке пунктов меню, а не отметок.
#[test]
fn marked_transports_keep_the_menu_order() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[
            ("transports", "ethernet"),
            ("transports", "uart"),
            ("transports", "готово"),
        ],
    )
    .expect("каскад не должен падать");
    let links = var_of(&result, "links").expect("links не задан");
    assert!(links.starts_with("uart:usart:USART1:"), "{links}");
    assert!(links.contains(",tcp:eth:ETH:"), "{links}");
}

/// Меню перерисовывается с отметкой выбранного пункта.
#[test]
fn marks_are_shown_in_the_redrawn_menu() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[("transports", "uart"), ("transports", "готово")],
    )
    .expect("каскад не должен падать");
    let shown: Vec<&Vec<String>> = result
        .prompts
        .iter()
        .filter(|(label, _)| label.starts_with("transports:"))
        .map(|(_, choices)| choices)
        .collect();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert!(shown[0][1].starts_with("[ ] uart "), "{shown:?}");
    assert!(shown[1][1].starts_with("[x] uart "), "{shown:?}");
    assert!(shown[1][2].starts_with("[ ] can "), "{shown:?}");
}

/// Отметить и снять тот же пункт — транспорта нет.
#[test]
fn toggling_twice_leaves_the_transport_out() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[
            ("transports", "can"),
            ("transports", "uart"),
            ("transports", "can"),
            ("transports", "готово"),
        ],
    )
    .expect("каскад не должен падать");
    assert_eq!(
        var_of(&result, "links"),
        Some("uart:usart:USART1:none:usart1")
    );
}

/// `готово` без отметок — проект без транспортов, о протоколе не спрашивают.
#[test]
fn done_without_marks_gives_no_transports() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[("transports", "готово")],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "links"), Some(""));
    assert_eq!(var_of(&result, "interface"), Some("none"));
    assert!(all_menu_values(&result, "protocol").is_empty());
}

/// Вопроса «добавить ещё один транспорт?» больше нет — ни в одном прогоне.
#[test]
fn more_links_is_gone() {
    let ast = compile_script();
    for picks in [
        &[][..],
        &[("transports", "uart")][..],
        &[
            ("transports", "uart"),
            ("transports", "can"),
            ("transports", "готово"),
            ("protocol", "modbus"),
        ][..],
    ] {
        let result = interview(&ast, "f407ve", &[], picks).expect("каскад не должен падать");
        assert!(
            all_menu_values(&result, "more_links").is_empty(),
            "{picks:?}"
        );
    }
}

/// Прежние переменные считаются от первого транспорта с протоколом.
#[test]
fn the_primary_link_is_the_first_with_a_protocol() {
    let result = run_chip_with(&compile_script(), "f407ve", &[("links", "can,uart:modbus")])
        .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "interface"), Some("uart"));
    assert_eq!(var_of(&result, "iface_periph"), Some("USART1"));
    assert_eq!(var_of(&result, "protocol"), Some("modbus"));
    assert_eq!(var_of(&result, "has_can"), Some("true"));
}

/// Блоки вида — все блоки всех его меток. Чипа с обеими метками одного вида
/// (`usb` и `otg`, `fdcan` и `bxcan`) в таблицах нет, поэтому проверяется
/// метка с двумя блоками: у F407 — `USB_OTG_FS` и `USB_OTG_HS`.
#[test]
fn both_tags_of_a_kind_count_as_its_blocks() {
    let result = run_chip_with(&compile_script(), "f407ve", &[("links", "usb,usb")])
        .expect("каскад не должен падать");
    assert_eq!(
        var_of(&result, "links"),
        Some("usb:otg:USB_OTG_FS:none:usb_otg_fs,usb:otg:USB_OTG_HS:none:usb_otg_hs")
    );
}

/// Слот сокета в стеке — у каждого сетевого транспорта, с протоколом и без: на
/// `tcp` без протокола приложение открывает свой сокет на `Board::net`.
#[test]
fn every_net_link_gets_a_socket_slot() {
    let ast = compile_script();
    for (links, net, protocol) in [("tcp", "1", "0"), ("tcp:modbus,udp", "2", "1")] {
        let result =
            run_chip_with(&ast, "f407ve", &[("links", links)]).expect("каскад не должен падать");
        assert_eq!(var_of(&result, "net_sockets"), Some(net), "{links}");
        assert_eq!(
            var_of(&result, "protocol_sockets"),
            Some(protocol),
            "{links}"
        );
    }
}

/// Производные переменные и удаление файлов смотрят на все транспорты, а не на
/// основной: RTU по `uart` и TCP по `tcp` в одном проекте, `can` без протокола.
#[test]
fn derived_flags_follow_every_link() {
    let result = run_chip_with(
        &compile_script(),
        "f407ve",
        &[("links", "uart:modbus,tcp:modbus,can")],
    )
    .expect("каскад не должен падать");
    for name in [
        "modbus_rtu",
        "modbus_tcp",
        "modbus_mbap",
        "modbus_stream",
        "has_can",
        "link_port",
        "stream_io",
    ] {
        assert_eq!(var_of(&result, name), Some("true"), "{name}");
    }
    for name in ["modbus_udp", "cobs_used", "has_protobuf"] {
        assert_eq!(var_of(&result, name), Some("false"), "{name}");
    }
    assert_eq!(var_of(&result, "protocol_sockets"), Some("1"));
    let deleted = |path: &str| result.deleted.iter().any(|d| d == path);
    for path in [
        "crates-cross/bsp/src/serial.rs",
        "crates-cross/bsp/src/can.rs",
        "crates-host/adapters/src/modbus/rtu.rs",
        "crates-host/adapters/src/modbus/tcp.rs",
    ] {
        assert!(!deleted(path), "{path} удалён: {:?}", result.deleted);
    }
    for path in [
        "crates-host/adapters/src/modbus/udp.rs",
        "crates-host/adapters/src/cobs.rs",
    ] {
        assert!(deleted(path), "{path} не удалён: {:?}", result.deleted);
    }
}

/// protobuf по UDP рядом с protobuf по потоку: COBS нужен потоку, хотя основной
/// транспорт — датаграммы.
#[test]
fn protobuf_over_udp_and_a_stream_keeps_cobs() {
    let result = run_chip_with(
        &compile_script(),
        "f407ve",
        &[("links", "udp:protobuf,uart:protobuf")],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "cobs_used"), Some("true"));
    assert!(
        !result
            .deleted
            .iter()
            .any(|d| d == "crates-host/adapters/src/cobs.rs"),
        "{:?}",
        result.deleted
    );
}

/// Modbus только по UDP — без потока: `modbus_stream` выключен.
#[test]
fn modbus_over_udp_alone_is_not_a_stream() {
    let result = run_chip_with(&compile_script(), "f407ve", &[("links", "udp:modbus")])
        .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "modbus_stream"), Some("false"));
    assert_eq!(var_of(&result, "modbus_udp"), Some("true"));
}

/// CANopen пока на одной шине: второй транспорт с ним — ошибка.
#[test]
fn a_second_canopen_bus_is_rejected() {
    let err = run_chip_with(
        &compile_script(),
        "f407ve",
        &[("links", "can:canopen,can:canopen")],
    )
    .err()
    .expect("второй CANopen принят");
    assert!(
        err.to_string()
            .contains("links: CANopen пока только на одной шине CAN"),
        "{err}"
    );
}

/// Второй `can` без протокола рядом с CANopen — через `--define links` он есть:
/// CANopen на первой шине, вторая — без протокола.
#[test]
fn a_second_can_without_a_protocol_sits_next_to_canopen() {
    let result = run_chip_with(&compile_script(), "f407ve", &[("links", "can:canopen,can")])
        .expect("каскад не должен падать");
    assert_eq!(
        var_of(&result, "links"),
        Some("can:bxcan:CAN1:canopen:can1,can:bxcan:CAN2:none:can2")
    );
}

/// Каскад не показывает чип, у которого блоков меньше, чем запрошено в `links`.
#[test]
fn chips_without_enough_blocks_are_filtered_before_the_cascade() {
    let script = compile_script();
    let err = run_cascade_with(&script, "f042c6", &[("links", "can,can")])
        .err()
        .expect("чип с одним CAN дошёл до конца каскада");
    assert!(err.to_string().contains("не ведёт к цели"), "{err}");
    run_cascade_with(&script, "f042c6", &[("links", "can")]).expect("один CAN у F042 есть");
    run_cascade_with(&script, "f407ve", &[("links", "can,can")]).expect("у F407 два CAN");
}

/// Ответы кончились — дальше как без терминала: `готово`, и отмеченное остаётся.
#[test]
fn no_terminal_adds_no_extra_links() {
    let result = interview(&compile_script(), "f407ve", &[], &[("transports", "uart")])
        .expect("каскад не должен падать");
    assert_eq!(
        var_of(&result, "links"),
        Some("uart:usart:USART1:none:usart1")
    );
    assert_eq!(all_menu_values(&result, "transports").len(), 2);
}

/// Без терминала список показан один раз, и проект — без транспортов.
#[test]
fn no_terminal_leaves_the_list_empty() {
    let result = interview(&compile_script(), "f407ve", &[], &[]).expect("каскад не должен падать");
    assert_eq!(all_menu_values(&result, "transports").len(), 1);
    assert_eq!(var_of(&result, "links"), Some(""));
    assert_eq!(var_of(&result, "protocol"), Some("none"));
}

/// `tcp` и `udp` делят один ETH: каждый вид получает его по разу, второй
/// транспорт того же вида — нет.
#[test]
fn tcp_and_udp_share_one_eth() {
    let script = compile_script();
    let result = run_chip_with(&script, "f407ve", &[("links", "tcp:modbus,udp:modbus")])
        .expect("tcp и udp на одном ETH должны приниматься");
    assert_eq!(
        var_of(&result, "links"),
        Some("tcp:eth:ETH:modbus:tcp,udp:eth:ETH:modbus:udp")
    );
    let err = run_chip_with(&script, "f407ve", &[("links", "tcp,tcp")])
        .err()
        .expect("второй tcp принят");
    assert!(err.to_string().contains("блоков вида tcp — 1"), "{err}");
}

/// Отмеченный `ethernet` — транспорт `tcp` на ETH.
#[test]
fn ethernet_item_becomes_tcp() {
    let result = interview(
        &compile_script(),
        "f407ve",
        &[],
        &[("transports", "ethernet"), ("transports", "готово")],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "links"), Some("tcp:eth:ETH:none:tcp"));
    // Вопрос о протоколе называет пункт, который отметили, а не вид.
    assert!(
        result
            .prompts
            .iter()
            .any(|(label, _)| label.starts_with("protocol: протокол связи поверх ethernet (TCP) ")),
        "{:?}",
        result.prompts
    );
}

/// Ключ ed25519 для ответов `ssh_key`.
const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl user@host";

/// Сетевые службы — тем же списком с отметками, что и транспорты, и только
/// при Ethernet; у SSH затем — способ входа и ключ.
#[test]
fn net_services_are_marked_after_ethernet() {
    let result = interview(
        &compile_script(),
        "h743zg",
        &[],
        &[
            ("transports", "ethernet"),
            ("transports", "готово"),
            ("net_services", "rest"),
            ("net_services", "ssh"),
            ("net_services", "готово"),
            ("ssh_auth", "key"),
            ("ssh_key", SSH_KEY),
        ],
    )
    .expect("каскад не должен падать");
    let menus = all_menu_values(&result, "net_services");
    assert_eq!(menus.len(), 3, "{menus:?}");
    assert_eq!(menus[0], ["готово", "ssh", "rest"]);
    // Порядок — пунктов меню, а не отметок.
    assert_eq!(var_of(&result, "net_services"), Some("ssh,rest"));
    assert_eq!(var_of(&result, "ssh_auth"), Some("key"));
    assert_eq!(var_of(&result, "ssh_key"), Some(SSH_KEY));
}

/// Без Ethernet служб не предлагают, переменные пустые.
#[test]
fn no_net_services_without_ethernet() {
    let result = interview(
        &compile_script(),
        "h743zg",
        &[],
        &[("transports", "uart"), ("transports", "готово")],
    )
    .expect("каскад не должен падать");
    assert!(all_menu_values(&result, "net_services").is_empty());
    assert_eq!(var_of(&result, "net_services"), Some(""));
    assert_eq!(var_of(&result, "ssh_auth"), Some(""));
    assert_eq!(var_of(&result, "ssh_key"), Some(""));
}

/// Без RNG SSH в списке нет, и хук говорит почему.
#[test]
fn ssh_is_not_offered_without_rng() {
    let result = interview(
        &compile_script(),
        "f107vc",
        &[],
        &[("transports", "ethernet"), ("transports", "готово")],
    )
    .expect("каскад не должен падать");
    assert_eq!(
        all_menu_values(&result, "net_services"),
        [["готово", "rest"]]
    );
    assert!(
        result
            .prints
            .iter()
            .any(|line| line.starts_with("SSH не предлагается")),
        "{:?}",
        result.prints
    );
}

/// SSH (~165 KiB кода) — только в разделе приложения не меньше
/// `SSH_MIN_PARTITION`: у stm32f407ve с OTA `ACTIVE` 128K — нет, без OTA —
/// есть; `--define` в тесный раздел — ошибка.
#[test]
fn ssh_needs_room_for_its_code() {
    let ast = compile_script();
    let picks = [("transports", "ethernet"), ("transports", "готово")];
    let tight = interview(&ast, "f407ve", &[], &picks).expect("каскад не должен падать");
    assert_eq!(
        all_menu_values(&tight, "net_services"),
        [["готово", "rest"]]
    );
    assert!(
        tight
            .prints
            .iter()
            .any(|line| line.starts_with("SSH не предлагается: под приложение отведено")),
        "{:?}",
        tight.prints
    );
    let roomy =
        interview(&ast, "f407ve", &[("ota", "no")], &picks).expect("каскад не должен падать");
    assert_eq!(
        all_menu_values(&roomy, "net_services"),
        [["готово", "ssh", "rest"]]
    );
    let Err(err) = interview(
        &ast,
        "f407ve",
        &[("links", "tcp"), ("net_services", "ssh")],
        &[],
    ) else {
        panic!("ssh в 128K должен падать");
    };
    assert!(err.to_string().contains("ssh не помещается"), "{err}");
}

/// Ключ хоста — в разделе настроек, только если тот настоящий и свободен:
/// внешняя флеш до драйвера проекта — заглушка, а CANopen забирает раздел
/// целиком; иначе — файл. Номера клиентов OTA служб — следом за транспортами.
#[test]
fn the_ssh_host_key_lives_where_it_survives() {
    let ast = compile_script();
    let host_key = |defines: &[(&str, &str)]| {
        let mut all = vec![("links", "tcp:modbus"), ("net_services", "rest,ssh")];
        all.extend_from_slice(defines);
        let result = interview(&ast, "h743zg", &all, &[]).expect("каскад не должен падать");
        let var = |name| var_of(&result, name).map(str::to_string);
        (
            var("ssh_host_key"),
            var("rest_ota_client"),
            var("ssh_ota_client"),
        )
    };
    let some = |s: &str| Some(s.to_string());
    assert_eq!(
        host_key(&[("config", "internal")]),
        (some("config"), some("1"), some("3"))
    );
    assert_eq!(host_key(&[("config", "external")]).0, some("file"));
    assert_eq!(host_key(&[]).0, some("file"));
}

/// Без терминала — без служб, о входе не спрашивают.
#[test]
fn no_terminal_gives_no_net_services() {
    let result = interview(
        &compile_script(),
        "h743zg",
        &[],
        &[("transports", "ethernet"), ("transports", "готово")],
    )
    .expect("каскад не должен падать");
    assert_eq!(all_menu_values(&result, "net_services").len(), 1);
    assert_eq!(var_of(&result, "net_services"), Some(""));
    assert!(all_menu_values(&result, "ssh_auth").is_empty());
}

/// Вход по паролю — ключ не спрашивают.
#[test]
fn password_login_asks_no_key() {
    let result = interview(
        &compile_script(),
        "h743zg",
        &[("links", "tcp"), ("net_services", "ssh")],
        &[("ssh_auth", "password")],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "ssh_auth"), Some("password"));
    assert_eq!(var_of(&result, "ssh_key"), Some(""));
    assert!(
        !result
            .prompts
            .iter()
            .any(|(label, _)| label.starts_with("ssh_key:"))
    );
}

/// Выбрали вход по ключу, а ключа под рукой нет: пустой ввод — вход по паролю.
#[test]
fn an_empty_key_falls_back_to_password() {
    let result = interview(
        &compile_script(),
        "h743zg",
        &[("links", "tcp"), ("net_services", "ssh")],
        &[("ssh_auth", "key"), ("ssh_key", "")],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&result, "ssh_auth"), Some("password"));
    assert_eq!(var_of(&result, "ssh_key"), Some(""));
}

/// `--define` строгий: всё, что службы сделать не могут, — ошибка с причиной.
#[test]
fn net_services_defines_are_strict() {
    let ast = compile_script();
    for (suffix, defines, expected) in [
        (
            "h743zg",
            &[("links", "tcp"), ("net_services", "telnet")][..],
            "net_services: ожидается",
        ),
        (
            "h743zg",
            &[("links", "uart"), ("net_services", "rest")][..],
            "работает поверх Ethernet",
        ),
        (
            "f107vc",
            &[("links", "tcp"), ("net_services", "ssh")][..],
            "нет RNG",
        ),
        // Вход по ключу без ключа (без терминала) и с ключом не ed25519.
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "ssh"),
                ("ssh_auth", "key"),
            ][..],
            "ssh_key: вход по ключу",
        ),
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "ssh"),
                ("ssh_auth", "key"),
                ("ssh_key", "ssh-rsa AAAA"),
            ][..],
            "ssh_key: вход по ключу",
        ),
        // Одно начало без тела ключа и кавычка, которая сломала бы литерал.
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "ssh"),
                ("ssh_auth", "key"),
                ("ssh_key", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5"),
            ][..],
            "ssh_key: вход по ключу",
        ),
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "ssh"),
                ("ssh_auth", "key"),
                (
                    "ssh_key",
                    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl a\"b",
                ),
            ][..],
            "ssh_key: вход по ключу",
        ),
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "ssh"),
                ("ssh_auth", "otp"),
            ][..],
            "ssh_auth: ожидается",
        ),
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "rest"),
                ("ssh_auth", "key"),
            ][..],
            "ssh_auth: задаётся только",
        ),
        (
            "h743zg",
            &[
                ("links", "tcp"),
                ("net_services", "ssh"),
                ("ssh_auth", "password"),
                ("ssh_key", SSH_KEY),
            ][..],
            "ssh_key: ключ нужен только",
        ),
    ] {
        let Err(err) = interview(&ast, suffix, defines, &[]) else {
            panic!("{defines:?} должен падать");
        };
        assert!(err.to_string().contains(expected), "{defines:?}: {err}");
    }
    let ok = interview(
        &ast,
        "h743zg",
        &[
            ("links", "tcp"),
            ("net_services", "rest, ssh"),
            ("ssh_auth", "key"),
            ("ssh_key", SSH_KEY),
        ],
        &[],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&ok, "net_services"), Some("rest,ssh"));
    assert_eq!(var_of(&ok, "ssh_key"), Some(SSH_KEY));
    // Без терминала вход — по паролю.
    let silent = interview(
        &ast,
        "h743zg",
        &[("links", "tcp"), ("net_services", "ssh")],
        &[],
    )
    .expect("каскад не должен падать");
    assert_eq!(var_of(&silent, "ssh_auth"), Some("password"));
}

/// Протоколу граф не нужен: без него узел спавнит `tasks::spawn_all`, и ни ответ `graph=no`,
/// ни размер раздела (здесь — 16 KiB у `l031f4`; у `l011f4` с тем же разделом Modbus не
/// помещается в 2 KiB RAM) генерацию не останавливают. Готовый
/// транспорт `app/src/link.rs` остаётся, пока выбран Modbus или protobuf.
#[test]
fn protocol_does_not_need_the_graph() {
    let script = compile_script();
    for (chip, preset) in [
        (
            "f407ve",
            &[
                ("protocol", "protobuf"),
                ("interface", "uart"),
                ("graph", "no"),
            ][..],
        ),
        (
            "l031f4",
            &[("protocol", "modbus"), ("interface", "uart")][..],
        ),
    ] {
        let result = run_cascade_with(&script, chip, preset)
            .unwrap_or_else(|err| panic!("{chip} {preset:?}: каскад упал: {err}"));
        assert_eq!(
            result.vars.get("graph").map(String::as_str),
            Some("false"),
            "{chip} {preset:?}"
        );
        assert!(
            !result
                .deleted
                .iter()
                .any(|path| path == "crates-cross/app/src/link.rs"),
            "{chip} {preset:?}: link.rs удалён при выбранном протоколе"
        );
    }

    let none = run_cascade_with(&script, "f407ve", &[]).expect("каскад не должен падать");
    assert!(
        none.deleted
            .iter()
            .any(|path| path == "crates-cross/app/src/link.rs"),
        "без протокола link.rs должен удаляться"
    );
}

/// Транспорт без протокола — рабочий выбор: заглушка интерфейса и его фоновый
/// узел (`NET`, `USB`) в проекте есть, а протокол — ваш (например, свой).
#[test]
fn a_transport_without_a_protocol_is_a_valid_project() {
    let ast = compile_script();
    for (chip, interface) in [
        ("g474re", "uart"),
        ("g474re", "can"),
        ("h743zg", "tcp"),
        ("h743zg", "udp"),
        ("g474re", "usb"),
    ] {
        let result = run_cascade_with(&ast, chip, &[("interface", interface)])
            .unwrap_or_else(|err| panic!("{chip} interface={interface}: каскад упал: {err}"));
        let var = |name: &str| result.vars.get(name).map(String::as_str);
        assert_eq!(var("interface"), Some(interface), "{chip}");
        assert_eq!(var("protocol"), Some("none"), "{chip} {interface}");
        // Кадровый канал нужен шине CAN и UDP, но не потоку.
        let link_gone = result
            .deleted
            .iter()
            .any(|d| d == "crates-host/ports/src/link.rs");
        assert_eq!(
            link_gone,
            !matches!(interface, "can" | "udp"),
            "{chip} {interface}"
        );
        // Заглушка шины CAN реализует порт `CanBus`, поэтому `ports::can` остаётся.
        let can_gone = result
            .deleted
            .iter()
            .any(|d| d == "crates-host/ports/src/can.rs");
        assert_eq!(can_gone, interface != "can", "{chip} {interface}");
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

    // h723ve схему не вмещает — без явного ответа OTA не предлагается по умолчанию.
    let no_room = run_chip_with(&ast, "h723ve", &[]).expect("каскад без места");
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
        let result = run_chip_with(&ast, suffix, &[("config", "internal")])
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
        // `--define` такого не принимает: sequential-storage там молча не заработал бы.
        let err = run_chip_with(&ast, suffix, &[("config", "internal")])
            .err()
            .unwrap_or_else(|| panic!("{suffix}: config=internal принят"));
        assert!(
            err.to_string().contains("флеш стирается в ноль"),
            "{suffix}: {err}"
        );
        // А меню его не предлагает вовсе.
        let result = run_interview(
            &ast,
            suffix,
            &[("chip_feature", &format!("stm32{suffix}"))],
            &[],
        )
        .expect("каскад не должен падать");
        let menu = result
            .prompts
            .iter()
            .find(|(label, _)| label.starts_with("config:"))
            .expect("вопрос про настройки не задан");
        assert!(
            !menu.1.iter().any(|choice| choice.starts_with("internal")),
            "{suffix}: {:?}",
            menu.1
        );
        assert_eq!(
            result.vars.get("config").map(String::as_str),
            Some("false"),
            "{suffix}"
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

/// Своя страница (`ErasePage`) у `STATE` и `CONFIG` нужна, только когда сектор
/// раздела мельче страницы цельного `Flash`: F2/F4/F7. На чипах с равными
/// секторами и при разделе настроек в хвосте flash обёртки нет.
#[test]
fn erase_page_wrapper_only_where_the_sector_is_smaller_than_the_chip_page() {
    let ast = compile_script();
    // (чип, ответы, state_paged, config_paged, boot_partitioned)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a str, &'a str, &'a str);
    let cases: [Case; 6] = [
        // F4: `STATE` в секторе 16 KiB при странице 128 KiB.
        ("f407ve", &[], "true", "false", "true"),
        // С разделом настроек: `STATE` в секторе 4 (64 KiB), `CONFIG` — в 16 KiB.
        ("f407ve", &[("config", "internal")], "true", "true", "true"),
        // Без OTA `CONFIG` — в хвосте, в секторе 128 KiB, как страница чипа.
        (
            "f407ve",
            &[("config", "internal"), ("ota", "no")],
            "false",
            "false",
            "false",
        ),
        // Равные секторы: раздел строится прямо над `Whole`.
        (
            "g071rb",
            &[("config", "internal")],
            "false",
            "false",
            "false",
        ),
        // `DFU` в банке 2: `STATE` по-прежнему равен странице чипа.
        ("h743zg", &[], "false", "false", "false"),
        // Страница обмена расширена: `ACTIVE`/`DFU` в `ErasePage`, а `STATE` — нет.
        ("l073rz", &[], "false", "false", "true"),
    ];
    for (chip, answers, state, config, boot) in cases {
        let result = run_cascade_with(&ast, chip, answers)
            .unwrap_or_else(|err| panic!("{chip} {answers:?}: каскад упал: {err}"));
        let var = |name: &str| result.vars.get(name).map(String::as_str);
        assert_eq!(
            var("state_paged"),
            Some(state),
            "{chip} {answers:?}: state_paged"
        );
        assert_eq!(
            var("config_paged"),
            Some(config),
            "{chip} {answers:?}: config_paged"
        );
        assert_eq!(
            var("boot_partitioned"),
            Some(boot),
            "{chip} {answers:?}: boot_partitioned"
        );
        // Тип `ErasePage` есть в проекте, только пока им пользуется хоть один раздел.
        let wrapped = state == "true" || config == "true" || var("swap_paged") == Some("true");
        assert_eq!(
            var("erase_page"),
            Some(if wrapped { "true" } else { "false" }),
            "{chip} {answers:?}: erase_page"
        );
    }
}

/// Транспорты в `adapters` остаются только под выбранный интерфейс: Modbus RTU,
/// TCP и UDP, сокеты сети и COBS — каждый лишь там, где ему есть кого обслуживать.
#[test]
fn transports_are_kept_only_for_the_selected_interface() {
    let ast = compile_script();
    // (чип, ответы, файлы, которые должны быть удалены, файлы, которые остаются)
    type Case<'a> = (
        &'a str,
        &'a [(&'a str, &'a str)],
        &'a [&'a str],
        &'a [&'a str],
    );
    const M: &str = "crates-host/adapters/src/modbus/";
    const N: &str = "crates-host/adapters/src/net/";
    const COBS: &str = "crates-host/adapters/src/cobs.rs";
    let cases: [Case; 5] = [
        (
            "g474re",
            &[("protocol", "modbus"), ("interface", "uart")],
            &["modbus/tcp.rs", "modbus/udp.rs", "modbus/mbap.rs"],
            &["modbus/rtu.rs"],
        ),
        (
            "h743zg",
            &[("protocol", "modbus"), ("interface", "udp")],
            &["modbus/rtu.rs", "modbus/tcp.rs", "net/tcp.rs"],
            &["modbus/udp.rs", "modbus/mbap.rs", "net/udp.rs"],
        ),
        (
            "h743zg",
            &[("protocol", "modbus"), ("interface", "tcp")],
            &["modbus/rtu.rs", "modbus/udp.rs", "net/udp.rs"],
            &["modbus/tcp.rs", "modbus/mbap.rs", "net/tcp.rs"],
        ),
        // COBS нужен потоку (UART, USB, TCP), а UDP кадр даёт сама датаграмма.
        (
            "f407ve",
            &[("protocol", "protobuf"), ("interface", "udp")],
            &["cobs.rs"],
            &[],
        ),
        (
            "f407ve",
            &[("protocol", "protobuf"), ("interface", "uart")],
            &[],
            &["cobs.rs"],
        ),
    ];
    for (chip, answers, gone, kept) in cases {
        let result = run_cascade_with(&ast, chip, answers)
            .unwrap_or_else(|err| panic!("{chip} {answers:?}: каскад упал: {err}"));
        let full = |short: &str| {
            if short == "cobs.rs" {
                COBS.to_owned()
            } else if let Some(rest) = short.strip_prefix("net/") {
                format!("{N}{rest}")
            } else {
                format!("{M}{}", short.trim_start_matches("modbus/"))
            }
        };
        for file in gone {
            assert!(
                result.deleted.iter().any(|d| *d == full(file)),
                "{chip} {answers:?}: {file} должен быть удалён"
            );
        }
        for file in kept {
            assert!(
                !result.deleted.iter().any(|d| *d == full(file)),
                "{chip} {answers:?}: {file} удалён, а нужен"
            );
        }
    }
}

/// OTA и хранилище настроек в `ports`, `adapters` и `domain` остаются только там,
/// где они выбраны: без них это код, до которого не дотянуться от `main` и графа.
/// Заглушка `Unavailable` нужна и узлу CANopen без хранилища, а `flash.rs` —
/// только обёртке страницы или внешней флеши.
#[test]
fn ota_and_settings_code_is_kept_only_when_selected() {
    let ast = compile_script();
    const OTA: [&str; 5] = [
        "crates-host/ports/src/ota.rs",
        "crates-host/ports/src/ota",
        "crates-host/adapters/src/ota.rs",
        "crates-host/domain/src/ota.rs",
        "crates-host/domain/src/ota",
    ];
    const SETTINGS: [&str; 2] = [
        "crates-host/ports/src/settings.rs",
        "crates-host/adapters/src/settings.rs",
    ];
    const FLASH: &str = "crates-host/flash-page";
    const WORD: &str = "crates-host/ports/src/word.rs";
    // (чип, ответы, OTA вырезана, настройки вырезаны, flash-page вырезан)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], bool, bool, bool);
    let cases: [Case; 5] = [
        (
            "g474re",
            &[("ota", "no"), ("config", "no")],
            true,
            true,
            true,
        ),
        (
            "g474re",
            &[("ota", "internal"), ("config", "no")],
            false,
            true,
            true,
        ),
        // CANopen без хранилища: порт и заглушка остаются, OTA — нет.
        (
            "g474re",
            &[("ota", "no"), ("protocol", "canopen"), ("interface", "can")],
            true,
            false,
            true,
        ),
        // Настройки в хвосте flash на равных секторах: обёртки страницы нет.
        (
            "g071rb",
            &[("ota", "no"), ("config", "internal")],
            true,
            false,
            true,
        ),
        // Внешняя флеш без OTA: заглушка драйвера — в `bsp::stub`, страница
        // раздела своя не нужна.
        (
            "l073rz",
            &[("ota", "no"), ("config", "external")],
            true,
            false,
            true,
        ),
    ];
    for (chip, answers, no_ota, no_settings, no_flash) in cases {
        let result = run_cascade_with(&ast, chip, answers)
            .unwrap_or_else(|err| panic!("{chip} {answers:?}: каскад упал: {err}"));
        let gone = |path: &str| result.deleted.iter().any(|d| d == path);
        for path in OTA {
            assert_eq!(gone(path), no_ota, "{chip} {answers:?}: {path}");
        }
        for path in SETTINGS {
            assert_eq!(gone(path), no_settings, "{chip} {answers:?}: {path}");
        }
        assert_eq!(gone(FLASH), no_flash, "{chip} {answers:?}: {FLASH}");
        // Слово записи читают OTA и узел CANopen; одни настройки — нет.
        let canopen = answers.contains(&("protocol", "canopen"));
        assert_eq!(gone(WORD), no_ota && !canopen, "{chip} {answers:?}: {WORD}");
    }
}

/// Сторож платы (`bsp::wdg`) нужен только графу, а кадровый канал
/// (`ports::link`) — только протоколам, которые им пользуются.
#[test]
fn watchdog_and_link_port_follow_the_choices() {
    let ast = compile_script();
    const WDG: [&str; 2] = ["crates-cross/bsp/src/wdg.rs", "docs/modules/bsp-wdg.md"];
    const LINK: &str = "crates-host/ports/src/link.rs";
    // (чип, ответы, сторож удалён, канал удалён)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], bool, bool);
    let cases: [Case; 7] = [
        ("g474re", &[], false, true),
        ("g474re", &[("graph", "no")], true, true),
        // Modbus по потоку кадровый канал не использует, по UDP — использует.
        (
            "g474re",
            &[("protocol", "modbus"), ("interface", "uart")],
            false,
            true,
        ),
        (
            "h743zg",
            &[("protocol", "modbus"), ("interface", "tcp")],
            false,
            true,
        ),
        (
            "h743zg",
            &[("protocol", "modbus"), ("interface", "udp")],
            false,
            false,
        ),
        (
            "f407ve",
            &[("protocol", "protobuf"), ("interface", "uart")],
            false,
            false,
        ),
        (
            "g474re",
            &[("protocol", "canopen"), ("interface", "can")],
            false,
            false,
        ),
    ];
    for (chip, answers, no_wdg, no_link) in cases {
        let result = run_cascade_with(&ast, chip, answers)
            .unwrap_or_else(|err| panic!("{chip} {answers:?}: каскад упал: {err}"));
        let gone = |path: &str| result.deleted.iter().any(|d| d == path);
        for path in WDG {
            assert_eq!(gone(path), no_wdg, "{chip} {answers:?}: {path}");
        }
        assert_eq!(gone(LINK), no_link, "{chip} {answers:?}: {LINK}");
    }
}

/// Имена переменных, которые читают Liquid-теги шаблона: `{{ name }}` и
/// `{% if name == ... %}` (в том числе второй и третий операнд `and`/`or`).
/// Хук обязан задать каждую на любом пути по меню — иначе подстановка в
/// `cargo generate` упадёт на неопределённой переменной.
fn template_variables() -> std::collections::BTreeSet<String> {
    template_variables_in(&repo_root())
}

/// То же по дереву `root`.
fn template_variables_in(root: &std::path::Path) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;

    fn walk(dir: &std::path::Path, found: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if matches!(
                    name.as_str(),
                    "target"
                        | ".git"
                        | "chip-data-gen"
                        | "chip-data"
                        | ".remember"
                        | ".claude"
                        | "superpowers"
                        | ".superpowers"
                        | "node_modules"
                ) {
                    continue;
                }
                walk(&path, found);
                continue;
            }
            // Сам хук и Rust-код генератора не шаблон: в них Liquid встречается как текст.
            if name.ends_with(".rhai") || name == "Cargo.lock" || name.ends_with(".lock") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            // Переменные файла и имена, которые он объявляет сам (`for`, `assign`,
            // `capture`): вторые — не переменные хука.
            let mut local = BTreeSet::new();
            let mut declared = BTreeSet::new();
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else { break };
                let inner = after[..end].trim().trim_start_matches('-').trim();
                let ident: String = inner
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !ident.is_empty() && !ident.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    local.insert(ident);
                }
                rest = &after[end + 2..];
            }
            let mut rest = text.as_str();
            while let Some(start) = rest.find("{%") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("%}") else { break };
                let tag = after[..end].trim().trim_start_matches('-').trim();
                for (keyword, sep) in [("for ", " in "), ("assign ", "="), ("capture ", "")] {
                    if let Some(body) = tag.strip_prefix(keyword) {
                        let name: String = body
                            .trim_start()
                            .chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .collect();
                        declared.insert(name);
                        // Первое имя правой части (`links` в `assign xs = links | split`,
                        // `net_sockets` в `for i in (1..net_sockets)`) — переменная
                        // хука, если файл не объявил его сам; строковый литерал — нет.
                        let Some((_, expr)) = body.split_once(sep).filter(|_| !sep.is_empty())
                        else {
                            continue;
                        };
                        let head = expr.split('|').next().unwrap_or_default().trim();
                        if head.starts_with(['"', '\'']) {
                            continue;
                        }
                        if let Some(first) = head
                            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                            .find(|w| !w.is_empty() && !w.starts_with(|c: char| c.is_ascii_digit()))
                        {
                            local.insert(first.to_string());
                        }
                    }
                }
                if let Some(cond) = tag
                    .strip_prefix("if ")
                    .or_else(|| tag.strip_prefix("elsif "))
                    .or_else(|| tag.strip_prefix("unless "))
                {
                    // Каждый операнд, за которым стоит оператор сравнения.
                    let words: Vec<&str> = cond.split_whitespace().collect();
                    for (i, word) in words.iter().enumerate() {
                        let is_op = matches!(
                            words.get(i + 1).copied(),
                            Some("==" | "!=" | ">" | "<" | ">=" | "<=" | "contains")
                        );
                        let bare = i == 0 && words.len() == 1;
                        if (is_op || bare)
                            && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && !word.chars().next().is_some_and(|c| c.is_ascii_digit())
                        {
                            local.insert((*word).to_string());
                        }
                    }
                }
                rest = &after[end + 2..];
            }
            found.extend(local.difference(&declared).cloned());
        }
    }

    let mut found = BTreeSet::new();
    walk(root, &mut found);
    found
}

/// Переменные циклов (`{% for l in xs %}`, `{% assign f = … %}`) объявляет сам
/// файл шаблона, а не хук: искать их среди переменных хука незачем. А вот
/// первое имя правой части (`links` в `assign xs = links | split`) хук задать
/// обязан, если файл не объявил его сам.
#[test]
fn loop_variables_are_not_hook_variables() {
    let dir = std::env::temp_dir().join(format!("loop-vars-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("временный каталог");
    std::fs::write(
        dir.join("t.rs"),
        "{%- assign xs = links | split: \",\" -%}{% for l in xs %}{{ l }}         {% assign f = l | split: \":\" %}{{ f[0] }}{% capture c %}x{% endcapture %}{{ c }}         {% endfor %}{{ chip }}",
    )
    .expect("временный файл");
    let found = template_variables_in(&dir);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(found.into_iter().collect::<Vec<_>>(), ["chip", "links"]);
}

/// Пункт меню без отметки `[x] `/`[ ] ` списка транспортов.
fn unmarked(choice: &str) -> &str {
    choice
        .strip_prefix("[x] ")
        .or_else(|| choice.strip_prefix("[ ] "))
        .unwrap_or(choice)
}

/// Значение варианта меню — его первое слово (у пункта с отметкой — после неё).
fn menu_value(choice: &str) -> &str {
    unmarked(choice)
        .split_whitespace()
        .next()
        .unwrap_or_default()
}

/// Значения вариантов меню `id` в том порядке, в каком их видит пользователь.
fn menu_values(result: &CascadeResult, id: &str) -> Option<Vec<String>> {
    result
        .prompts
        .iter()
        .find(|(label, _)| label.starts_with(&format!("{id}:")))
        .map(|(_, choices)| choices.iter().map(|c| menu_value(c).to_string()).collect())
}

/// Чип задан напрямую, а вопросы про OTA, транспорт и протокол — через меню.
fn interview(
    ast: &Script,
    suffix: &str,
    defines: &[(&str, &str)],
    picks: &[(&str, &str)],
) -> Result<CascadeResult, Box<EvalAltResult>> {
    let chip = format!("stm32{suffix}");
    let mut all = defines.to_vec();
    all.push(("chip_feature", chip.as_str()));
    run_interview(ast, suffix, &all, picks)
}

/// Вопросы идут в одном порядке и предлагают только допустимое: сначала OTA,
/// настройки и граф, затем транспорт, и только потом — протоколы этого транспорта.
#[test]
fn the_menus_come_in_order_and_offer_only_what_is_possible() {
    let ast = compile_script();

    // Порядок: транспорт раньше протокола, master — только после Modbus.
    let result = interview(
        &ast,
        "g474re",
        &[],
        &[
            ("transports", "uart"),
            ("transports", "готово"),
            ("protocol", "modbus"),
        ],
    )
    .expect("каскад не должен падать");
    let order: Vec<&str> = result
        .prompts
        .iter()
        .map(|(label, _)| label.split(':').next().unwrap_or_default())
        .collect();
    assert_eq!(
        order,
        [
            "ota",
            "config",
            "graph",
            "transports",
            "transports",
            "protocol",
            "modbus_master"
        ]
    );

    // Транспорт: только то, чьи блоки есть у чипа. У G071 нет ни Ethernet, ни USB.
    let g071 = interview(&ast, "g071rb", &[], &[]).expect("каскад не должен падать");
    let interfaces = menu_values(&g071, "transports").expect("транспорт не предложен");
    assert_eq!(interfaces, ["готово", "uart"]);
    // Без транспорта протокол не спрашивается вовсе.
    assert!(menu_values(&g071, "protocol").is_none());

    // Протоколы — только те, что работают поверх выбранного транспорта.
    for (chip, interface, expected) in [
        ("g474re", "uart", &["none", "modbus", "protobuf"][..]),
        ("g474re", "can", &["none", "canopen"][..]),
        ("h743zg", "ethernet", &["none", "modbus", "protobuf"][..]),
    ] {
        let result = interview(&ast, chip, &[], &[("transports", interface)])
            .expect("каскад не должен падать");
        assert_eq!(
            menu_values(&result, "protocol").expect("протокол не предложен"),
            expected,
            "{chip} {interface}"
        );
    }

    // Master — вопрос только про Modbus.
    let protobuf = interview(
        &ast,
        "g474re",
        &[],
        &[("transports", "uart"), ("protocol", "protobuf")],
    )
    .expect("каскад не должен падать");
    assert!(menu_values(&protobuf, "modbus_master").is_none());

    // Граф, который не влезает, не предлагается вовсе; в просторном чипе — да.
    let tiny = interview(&ast, "l011f4", &[], &[]).expect("каскад не должен падать");
    assert!(menu_values(&tiny, "graph").is_none());
    assert_eq!(tiny.vars.get("graph").map(String::as_str), Some("false"));
    let roomy = interview(&ast, "f407ve", &[], &[]).expect("каскад не должен падать");
    assert_eq!(
        menu_values(&roomy, "graph"),
        Some(vec!["yes".into(), "no".into()])
    );
}

/// Выбор в меню доходит до переменных, которые читают шаблоны.
#[test]
fn menu_picks_reach_the_template_variables() {
    let ast = compile_script();
    let result = interview(
        &ast,
        "h743zg",
        &[],
        &[
            ("ota", "internal"),
            ("config", "no"),
            ("graph", "no"),
            ("transports", "ethernet"),
            ("protocol", "modbus"),
            ("modbus_master", "yes"),
        ],
    )
    .expect("каскад не должен падать");
    let var = |name: &str| result.vars.get(name).map(String::as_str);
    assert_eq!(var("ota"), Some("true"));
    assert_eq!(var("config"), Some("false"));
    assert_eq!(var("graph"), Some("false"));
    assert_eq!(var("interface"), Some("tcp"));
    assert_eq!(var("protocol"), Some("modbus"));
    assert_eq!(var("modbus_master"), Some("true"));
}

/// Без терминала и без `--define` вопросы получают умолчания, и генерация не
/// падает ни на одном чипе: ни одного вопроса, на который некому ответить.
#[test]
fn every_chip_generates_with_the_defaults() {
    let ast = compile_script();
    let mut failures = Vec::new();
    for suffix in all_chip_suffixes() {
        match interview(&ast, &suffix, &[], &[]) {
            Err(err) => failures.push(format!("{suffix}: {err}")),
            Ok(result) => {
                let var = |name: &str| result.vars.get(name).map(String::as_str);
                if var("protocol") != Some("none") || var("interface") != Some("none") {
                    failures.push(format!(
                        "{suffix}: умолчание — не «без протокола и транспорта»"
                    ));
                }
                // Умолчание не обещает невозможного: OTA — только если помещается.
                if var("ota") == Some("true")
                    && !result.files.contains_key("crates-cross/boot/memory.x")
                {
                    failures.push(format!(
                        "{suffix}: OTA по умолчанию, а раскладка не посчитана"
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} чипов:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Обход всех путей по меню на характерных чипах: каждый путь, который меню
/// вообще позволяет пройти, даёт допустимые ответы и не роняет генерацию.
#[test]
fn every_path_through_the_menus_generates() {
    let ast = compile_script();
    // Разные семейства и размеры: крошечный, с EEPROM, неравные сектора, второй
    // банк, Ethernet, CAN двух видов, USB, стирание в ноль.
    let chips = [
        "l011f4",
        "l073rz",
        "g071rb",
        "g474re",
        "f407ve",
        "f429zg",
        "h743zg",
        "h723ve",
        "wb15cc",
        "c011d6",
        "h745zi-cm7",
    ];
    let used = template_variables();
    // Задаёт `cargo generate`, а не хук: вопрос `ci` и встроенные переменные.
    // `github` и `secrets` — не переменные шаблона, а выражения GitHub Actions
    // (`${{ secrets.X }}`), которые Liquid оставляет как есть.
    let external = [
        "ci",
        "project",
        "crate_name",
        "authors",
        "username",
        "os",
        "github",
        "secrets",
    ];
    let mut paths = 0;
    let mut failures = Vec::new();
    for chip in chips {
        // Ввод строки (`ssh_key:`) — не меню, перебирать в нём нечего: ответ
        // готов заранее и расходуется, только если вопрос задан.
        let mut stack: Vec<Vec<(String, String)>> =
            vec![vec![("ssh_key".to_string(), SSH_KEY.to_string())]];
        while let Some(picks) = stack.pop() {
            let refs: Vec<(&str, &str)> = picks
                .iter()
                .map(|(id, value)| (id.as_str(), value.as_str()))
                .collect();
            let result = match interview(&ast, chip, &[], &refs) {
                Ok(result) => result,
                Err(err) => {
                    failures.push(format!("{chip} {picks:?}: {err}"));
                    continue;
                }
            };
            let next = result.prompts.iter().find(|(label, _)| {
                let id = label.split(':').next().unwrap_or_default();
                !picks.iter().any(|(pick_id, _)| pick_id == id)
            });
            if let Some((label, choices)) = next {
                let id = label.split(':').next().unwrap_or_default().to_string();
                for choice in choices {
                    let mut deeper = picks.clone();
                    deeper.push((id.clone(), menu_value(choice).to_string()));
                    stack.push(deeper);
                }
                continue;
            }
            paths += 1;
            let var = |name: &str| result.vars.get(name).map(String::as_str);
            let missing: Vec<&String> = used
                .iter()
                .filter(|name| {
                    !result.vars.contains_key(name.as_str()) && !external.contains(&name.as_str())
                })
                .collect();
            if !missing.is_empty() {
                failures.push(format!("{chip} {picks:?}: не заданы {missing:?}"));
            }
            let protocol = var("protocol").unwrap_or("?");
            let interface = var("interface").unwrap_or("?");
            let allowed: &[&str] = match protocol {
                "none" => &["none", "uart", "can", "tcp", "udp", "usb"],
                "modbus" | "protobuf" => &["uart", "tcp", "udp", "usb"],
                "canopen" => &["can"],
                _ => &[],
            };
            if !allowed.contains(&interface) {
                failures.push(format!("{chip} {picks:?}: {protocol} поверх {interface}"));
            }
            for name in ["ota", "config", "graph", "modbus_master"] {
                if !matches!(var(name), Some("true" | "false")) {
                    failures.push(format!("{chip} {picks:?}: {name} = {:?}", var(name)));
                }
            }
            // Ответ дошёл до результата.
            for (id, value) in &picks {
                let expected = match (id.as_str(), value.as_str()) {
                    ("ota", "no") => Some(("ota", "false")),
                    ("config", "no") => Some(("config", "false")),
                    ("graph", "no") => Some(("graph", "false")),
                    ("protocol", v) => Some(("protocol", v)),
                    _ => None,
                };
                if let Some((name, want)) = expected
                    && var(name) != Some(want)
                {
                    failures.push(format!(
                        "{chip} {picks:?}: {name} = {:?}, а выбрано {want}",
                        var(name)
                    ));
                }
            }
        }
    }
    println!("путей по меню: {paths}");
    assert!(paths > 300, "обход слишком короткий: {paths}");
    assert!(
        failures.is_empty(),
        "{} путей:\n{}",
        failures.len(),
        failures.join("\n")
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

/// Кэши (`cache`): теги из метаданных — `icen`/`dcen` по полям `FLASH_ACR`,
/// `icache` по блоку `ICACHE`, — а `scb` добавляет ядро Cortex-M7. Где кэша нет
/// (F1, L0), `bsp/src/cache.rs` удаляется.
#[test]
fn caches_follow_the_metadata_and_the_core() {
    let ast = compile_script();
    for (suffix, expected) in [
        ("f407ve", "icen dcen"),
        ("g474re", "icen dcen"),
        ("g071rb", "icen"),
        ("l552ze", "icache"),
        ("u575zi", "icache"),
        ("f746ng", "scb scb_d"),
        ("h743zi", "scb scb_d"),
        ("h745zi-cm7", "scb scb_d"),
        ("h7s7l8", "scb scb_d"),
        ("h745zi-cm4", ""),
        ("f103c8", ""),
        ("l053r8", ""),
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("cache").map(String::as_str),
            Some(expected),
            "{suffix}"
        );
        assert_eq!(
            result
                .deleted
                .iter()
                .any(|path| path == "crates-cross/bsp/src/cache.rs"),
            expected.is_empty(),
            "{suffix}: cache.rs"
        );
    }
}

/// `DMA_NC` — non-cacheable память под буферы DMA: она есть у каждого чипа с ядром
/// Cortex-M7 (F7, H7, H7R/S; у CM4 двухъядерных H7 ядра M7 нет), лежит на границе
/// своего размера (требование региона MPU), не пересекается с `RAM` и `PANIC`, и только с
/// ней кэш данных ядра включается (`scb_d`). У H7 `SRAM4` целиком размечается под
/// BDMA. Секции под оба региона — в `memory.x` приложения.
#[test]
fn every_m7_chip_gets_a_dma_region() {
    fn region(memory_x: &str, name: &str) -> Option<(u64, u64)> {
        let line = memory_x
            .lines()
            .find(|line| line.trim_start().starts_with(name) && line.contains("ORIGIN"))?;
        let origin = line.split("ORIGIN = ").nth(1)?.split(',').next()?;
        let length = line.split("LENGTH = ").nth(1)?.trim();
        let origin = u64::from_str_radix(origin.trim_start_matches("0x"), 16).ok()?;
        let length = match length.strip_suffix('K') {
            Some(kib) => kib.parse::<u64>().ok()? * 1024,
            None => length.parse().ok()?,
        };
        Some((origin, length))
    }

    let ast = compile_script();
    let mut m7_chips = 0;
    let mut failures = Vec::new();
    for suffix in all_chip_suffixes() {
        let Ok(result) = run_cascade_to(&ast, &suffix) else {
            continue;
        };
        let Some(app) = result.files.get("crates-cross/app/memory.x") else {
            continue;
        };
        let memory_x = app.join("\n");
        let var = |name: &str| result.vars.get(name).map_or("", String::as_str);
        let m7 = var("cpu") == "cortex-m7";
        let dma = region(&memory_x, "DMA_NC");

        if !m7 {
            if dma.is_some() || var("cache").contains("scb") || !var("dma_nc_start").is_empty() {
                failures.push(format!(
                    "{suffix}: не M7, а регион DMA_NC или кэш ядра есть"
                ));
            }
            continue;
        }
        m7_chips += 1;
        let Some((origin, length)) = dma else {
            failures.push(format!("{suffix}: у M7 нет региона DMA_NC"));
            continue;
        };
        if !length.is_power_of_two() || length < 4096 || origin % length != 0 {
            failures.push(format!(
                "{suffix}: DMA_NC {origin:#x}+{length} не по границе размера"
            ));
        }
        if var("dma_nc_start") != origin.to_string() || var("dma_nc_len") != length.to_string() {
            failures.push(format!("{suffix}: dma_nc_start/len не сходятся с memory.x"));
        }
        if !var("cache").contains("scb_d") {
            failures.push(format!(
                "{suffix}: M7 с DMA_NC без кэша данных ({:?})",
                var("cache")
            ));
        }
        // Не пересекается с RAM и PANIC.
        for other in ["RAM", "PANIC"] {
            if let Some((o_origin, o_length)) = region(&memory_x, other)
                && o_origin < origin + length
                && origin < o_origin + o_length
            {
                failures.push(format!("{suffix}: DMA_NC пересекается с {other}"));
            }
        }
        if !memory_x.contains(".dma_nc (NOLOAD)") || !memory_x.contains("} > DMA_NC") {
            failures.push(format!("{suffix}: в memory.x нет секции .dma_nc"));
        }
        // SRAM4 под BDMA: только у H7 с SRAM4 в D3 (0x38000000).
        let bdma = region(&memory_x, "SRAM4").filter(|&(o, _)| o == 0x3800_0000);
        let h7_with_sram4 = suffix.starts_with("h7") && bdma.is_some();
        if h7_with_sram4 != !var("bdma_nc_start").is_empty()
            || h7_with_sram4 != memory_x.contains(".bdma_nc (NOLOAD)")
        {
            failures.push(format!("{suffix}: SRAM4 под BDMA и memory.x не сходятся"));
        }
        if let Some((o, l)) = bdma
            && h7_with_sram4
            && (var("bdma_nc_start") != o.to_string() || var("bdma_nc_len") != l.to_string())
        {
            failures.push(format!("{suffix}: bdma_nc_start/len не сходятся с SRAM4"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(m7_chips > 200, "чипов с ядром M7 всего {m7_chips}");
}

/// `core_baseline` — ядра без `DWT.CYCCNT`, `CFSR` и ловушки деления (M0, M0+,
/// M23): им `bsp` не собирает `cycles`, а `ports` — `CycleCounter`.
#[test]
fn baseline_cores_are_marked() {
    let ast = compile_script();
    for (suffix, expected) in [
        ("g071rb", true),  // Cortex-M0+
        ("f030c8", true),  // Cortex-M0
        ("f103c8", false), // Cortex-M3
        ("f407ve", false), // Cortex-M4
        ("h743zi", false), // Cortex-M7
        ("l552ze", false), // Cortex-M33
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("core_baseline").map(String::as_str),
            Some(if expected { "true" } else { "false" }),
            "{suffix}"
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
            let Ok(result) = run_chip_with(&ast, &suffix, preset) else {
                continue; // падения каскада ловят соседние тесты
            };
            if result.vars.get("ota").map(String::as_str) != Some("true") {
                continue;
            }
            // Ручная раскладка (выбранное на чип не помещается) memory.x не пишет.
            if !result.files.contains_key("crates-cross/boot/memory.x") {
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

/// Ответы отсекают чипы ещё до каскада: сетевой стек просит Ethernet, OTA и
/// граф — места во flash. Каскад до такого чипа не доходит, и пользователь
/// видит только то, с чем ответы выполнимы.
#[test]
fn answers_cut_the_chip_list_before_the_cascade() {
    let ast = compile_script();
    let net = [("protocol", "modbus"), ("interface", "tcp")];

    // С Ethernet — доходит, и хук сообщает, сколько чипов осталось.
    let ok = run_cascade_with(&ast, "f407ve", &net).expect("f407ve годится под сеть");
    assert!(
        ok.prints.iter().any(|line| line.contains("подходит")),
        "хук не сказал, что список сужен: {:?}",
        ok.prints
    );

    // Без Ethernet — каскад не может привести к чипу.
    let err = run_cascade_with(&ast, "g474re", &net)
        .err()
        .expect("g474re без Ethernet не должен быть в списке");
    assert!(err.to_string().contains("не ведёт к цели"), "{err}");

    // OTA, которая не помещается, и граф, который не влезает, — тоже: каскад
    // либо не может привести к чипу, либо приводит к соседу, которому место
    // есть (у h723ve это h723vg — 1 MiB).
    for (chip, preset) in [
        ("h723ve", [("ota", "internal"), ("graph", "no")]),
        ("l011f4", [("ota", "no"), ("graph", "yes")]),
    ] {
        let target = format!("stm32{chip}");
        match run_cascade_with(&ast, chip, &preset) {
            Err(err) => {
                assert!(err.to_string().contains("не ведёт к цели"), "{chip}: {err}");
            }
            Ok(result) => assert_ne!(
                result.vars.get("chip").map(String::as_str),
                Some(target.as_str()),
                "{chip} {preset:?}: чип не должен быть в списке"
            ),
        }
    }

    // Смягчили ответы — тот же чип снова доступен.
    for (chip, preset) in [
        ("h723ve", [("ota", "no"), ("graph", "no")]),
        ("l011f4", [("ota", "no"), ("graph", "no")]),
    ] {
        run_cascade_with(&ast, chip, &preset)
            .unwrap_or_else(|err| panic!("{chip} {preset:?}: {err}"));
    }
}

/// Тексты пунктов списка транспортов в каждом показе — по порядку.
fn transport_menus(result: &CascadeResult) -> Vec<&Vec<String>> {
    result
        .prompts
        .iter()
        .filter(|(label, _)| label.starts_with("transports:"))
        .map(|(_, choices)| choices)
        .collect()
}

/// Пункт `value` в показе меню `shown`.
fn item<'a>(shown: &'a [String], value: &str) -> &'a str {
    shown
        .iter()
        .find(|choice| menu_value(choice) == value)
        .unwrap_or_else(|| panic!("в меню нет пункта {value}: {shown:?}"))
}

const NO_RAM: &str = "НЕ ПОМЕЩАЕТСЯ в RAM";

/// `ram_estimate` — байты статической RAM, с транспортом их больше; хук печатает
/// её строкой `ram_estimate:` (её читает `template-check`).
#[test]
fn the_estimate_is_exported() {
    let ast = compile_script();
    let estimate = |preset: &[(&str, &str)]| -> u64 {
        let result = run_chip_with(&ast, "f407ve", preset).expect("каскад не должен падать");
        assert!(
            result
                .prints
                .iter()
                .any(|line| line.starts_with("ram_estimate: ")),
            "{:?}",
            result.prints
        );
        var_of(&result, "ram_estimate")
            .expect("ram_estimate не задан")
            .parse()
            .expect("ram_estimate — число")
    };
    let bare = estimate(&[]);
    assert!(bare > 0);
    assert!(estimate(&[("links", "uart:modbus")]) > bare);
    assert!(
        estimate(&[("links", "uart:modbus,tcp:modbus")]) > estimate(&[("links", "uart:modbus")])
    );
}

/// `--define links` с тем, что не помещается в RAM чипа, — ошибка с оценкой; до
/// каскада такой чип не показывается вовсе.
#[test]
fn define_links_that_do_not_fit_are_rejected() {
    let ast = compile_script();
    let err = run_chip_with(&ast, "f042f6", &[("links", "usb:protobuf")])
        .err()
        .expect("USB с protobuf в 5.6 KiB RAM принят");
    let text = err.to_string();
    assert!(
        text.contains("links: usb:protobuf не помещается в RAM STM32F042F6 (оценка "),
        "{text}"
    );
    let err = run_chip_with(
        &ast,
        "f042f6",
        &[("interface", "usb"), ("protocol", "protobuf")],
    )
    .err()
    .expect("interface=usb с protobuf в 5.6 KiB RAM принят");
    assert!(err.to_string().contains("interface: usb"), "{err}");

    match run_cascade_with(&ast, "f042f6", &[("links", "usb:protobuf")]) {
        Err(err) => assert!(err.to_string().contains("не ведёт к цели"), "{err}"),
        Ok(result) => assert_ne!(var_of(&result, "chip"), Some("STM32F042F6")),
    }
    run_cascade_with(&ast, "f072c8", &[("links", "usb:protobuf")])
        .expect("f072c8 (15.5 KiB RAM) должен остаться в каскаде");
    // Проект без транспортов не отвергается даже там, где база с запасом под
    // стек больше RAM.
    run_chip_with(&ast, "l011f4", &[]).expect("l011f4 без транспортов");
}

/// Пара `interface` + `protocol` отсеивает чипы по RAM до каскада так же, как
/// `links`: USB с protobuf STM32F042F6 не показывается, STM32F072C8 — остаётся.
/// Без `protocol` оценка — по транспорту без протокола, и F042F6 USB берёт.
#[test]
fn define_interface_and_protocol_filter_by_ram_before_the_cascade() {
    let ast = compile_script();
    let pair = [("interface", "usb"), ("protocol", "protobuf")];
    match run_cascade_with(&ast, "f042f6", &pair) {
        Err(err) => assert!(err.to_string().contains("не ведёт к цели"), "{err}"),
        Ok(result) => assert_ne!(var_of(&result, "chip"), Some("STM32F042F6")),
    }
    run_cascade_with(&ast, "f072c8", &pair)
        .expect("f072c8 (15.5 KiB RAM) должен остаться в каскаде");
    let result = run_cascade_with(&ast, "f042f6", &[("interface", "usb")])
        .expect("USB без протокола у f042f6 помещается");
    assert_eq!(var_of(&result, "chip"), Some("STM32F042F6"));
}

/// Ручная раскладка (`write_size`): регион RAM пишет человек — оценки нет,
/// меню без пометок, `--define` не отвергается.
#[test]
fn manual_layout_skips_the_ram_estimate() {
    let ast = compile_script();
    let result = run_chip_with(
        &ast,
        "f042f6",
        &[("write_size", "4"), ("links", "usb:protobuf")],
    )
    .expect("ручная раскладка не должна отвергаться по RAM");
    assert_eq!(var_of(&result, "ram_estimate"), Some(""));
    let result =
        interview(&ast, "f042f6", &[("write_size", "4")], &[]).expect("каскад не должен падать");
    for shown in transport_menus(&result) {
        assert!(
            shown.iter().all(|choice| !choice.contains(NO_RAM)),
            "{shown:?}"
        );
    }
}

/// Маленькая RAM (F0, 5.6 KiB): USB помечен, но выбрать его можно — с
/// предупреждением, если не поместился и выбранный протокол. У F407 пометок нет.
#[test]
fn a_transport_that_does_not_fit_is_marked() {
    let ast = compile_script();
    let result = interview(
        &ast,
        "f042f6",
        &[],
        &[
            ("transports", "usb"),
            ("transports", "готово"),
            ("protocol", "protobuf"),
        ],
    )
    .expect("помеченный пункт можно выбрать");
    let menus = transport_menus(&result);
    let usb = item(menus[0], "usb");
    assert!(
        usb.contains(&format!("{NO_RAM} (оценка ")) && usb.ends_with(" из 5.6 KiB)"),
        "{usb}"
    );
    assert!(
        result
            .prints
            .iter()
            .any(|line| line.contains("не помещается в RAM STM32F042F6")),
        "{:?}",
        result.prints
    );

    let result = interview(&ast, "f407ve", &[], &[]).expect("каскад не должен падать");
    for shown in transport_menus(&result) {
        assert!(
            shown.iter().all(|choice| !choice.contains(NO_RAM)),
            "{shown:?}"
        );
    }
}

/// Пометки считаются заново при каждой перерисовке: у F072RB (15.5 KiB RAM) USB,
/// UART и CAN по отдельности помещаются, а все три — нет; снятая отметка
/// возвращает место. Запас — 144 байта сверх RAM у трёх пунктов и 3.4 KiB у двух:
/// выросла таблица `RAM_COST` — нужен чип с OTA и графом, у которого
/// `база + usb + uart + 1024 <= ram_length < то же + can` (худшие протоколы видов),
/// перебором по `chip-data/layout/*.rhai` и `chip-data/interfaces.rhai`.
#[test]
fn marks_are_recomputed_after_each_toggle() {
    let result = interview(
        &compile_script(),
        "f072rb",
        &[],
        &[
            ("transports", "usb"),
            ("transports", "uart"),
            ("transports", "can"),
            ("transports", "uart"),
            ("transports", "готово"),
        ],
    )
    .expect("каскад не должен падать");
    let menus = transport_menus(&result);
    assert_eq!(menus.len(), 5, "{menus:?}");
    let marked = |n: usize, value: &str| item(menus[n], value).contains(NO_RAM);
    assert!(
        !marked(0, "usb") && !marked(0, "uart") && !marked(0, "can"),
        "{:?}",
        menus[0]
    );
    // USB и UART отмечены — CAN к ним уже не помещается.
    assert!(
        !marked(2, "usb") && !marked(2, "uart") && marked(2, "can"),
        "{:?}",
        menus[2]
    );
    // Отмечены все три — помечены все.
    assert!(
        marked(3, "usb") && marked(3, "uart") && marked(3, "can"),
        "{:?}",
        menus[3]
    );
    // UART снят — USB и CAN снова помещаются, UART к ним — нет.
    assert!(
        !marked(4, "usb") && marked(4, "uart") && !marked(4, "can"),
        "{:?}",
        menus[4]
    );
}

/// Отсев до каскада по `links` считает базу только из заданных ответов: STM32F100RB
/// (7.5 KiB RAM) с OTA и графом protobuf по UART не берёт, а без OTA — берёт, и
/// умолчание «да» для незаданного `ota` чип не прячет. Невлезающее с итоговыми
/// ответами отвергает проверка после них.
#[test]
fn the_link_filter_does_not_assume_unasked_answers() {
    let ast = compile_script();
    let links = ("links", "uart:protobuf");
    let result = run_interview(&ast, "f100rb", &[links], &[("ota", "no")])
        .expect("f100rb без OTA должен остаться в каскаде");
    assert_eq!(var_of(&result, "chip"), Some("STM32F100RB"));
    // Без терминала OTA по умолчанию есть — и тогда не помещается.
    let err = run_cascade_with(&ast, "f100rb", &[links])
        .err()
        .expect("protobuf по UART с OTA в 7.5 KiB RAM принят");
    assert!(
        err.to_string()
            .contains("links: uart:protobuf не помещается в RAM STM32F100RB"),
        "{err}"
    );
    // Заданные `ota=internal` и `graph=yes` в базе учитываются — чип не показывается.
    let asked = [links, ("ota", "internal"), ("graph", "yes")];
    match run_cascade_with(&ast, "f100rb", &asked) {
        Err(err) => assert!(err.to_string().contains("не ведёт к цели"), "{err}"),
        Ok(result) => assert_ne!(var_of(&result, "chip"), Some("STM32F100RB")),
    }
    // С ручной раскладкой оценки нет и в отсеве.
    run_cascade_with(
        &ast,
        "f100rb",
        &[
            links,
            ("ota", "internal"),
            ("graph", "yes"),
            ("write_size", "4"),
        ],
    )
    .expect("ручная раскладка не отсеивается по RAM");
}

/// Меню протокола помечает протоколы, с которыми транспорт не помещается: у
/// STM32F042F6 (5.6 KiB) USB без протокола помещается, с Modbus и protobuf — нет.
#[test]
fn protocols_that_do_not_fit_are_marked() {
    let result = interview(
        &compile_script(),
        "f042f6",
        &[],
        &[("transports", "usb"), ("transports", "готово")],
    )
    .expect("каскад не должен падать");
    let shown = &result
        .prompts
        .iter()
        .find(|(label, _)| label.starts_with("protocol:"))
        .expect("меню протокола не показано")
        .1;
    assert!(!item(shown, "none").contains(NO_RAM), "{shown:?}");
    assert!(item(shown, "modbus").contains(NO_RAM), "{shown:?}");
    assert!(item(shown, "protobuf").contains(NO_RAM), "{shown:?}");
}
