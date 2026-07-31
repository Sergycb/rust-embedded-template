//! Прогоняет реальный `chip-select.rhai` через движок Rhai с подменённым
//! `variable::*` — без запуска `cargo generate` и без TTY. `variable::prompt`
//! в моке сам идёт к заранее известной цели (выбирает вариант, префиксом
//! которого является целевой суффикс), так что тест проверяет тот же путь,
//! что реальный интерактивный каскад, а не только `--define chip_feature=...`
//! (та ветка не создаёт ни одного `variable::prompt` вызова и потому не может
//! поймать баги вроде бесконечного цикла в самом каскаде).

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use rhai::{AST, Array, Dynamic, Engine, EvalAltResult, Module, Scope};

fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("chip-data-gen должен лежать внутри репозитория")
        .join("chip-select.rhai")
}

fn read_script() -> String {
    std::fs::read_to_string(script_path()).expect("не удалось прочитать chip-select.rhai")
}

/// Парсинг ~1600-строчного скрипта — основная стоимость прогона; компилируем
/// один раз на вызывающий тест, а не при каждом обращении к `run_cascade_to`
/// (для `cascade_reaches_every_chip_without_hanging` — 1438 раз).
fn compile_script() -> AST {
    Engine::new()
        .compile(read_script())
        .expect("chip-select.rhai не парсится")
}

type Vars = Rc<RefCell<HashMap<String, String>>>;

/// Прогоняет уже скомпилированный chip-select.rhai один раз, ведя каскад к
/// `target_suffix` (суффикс без "stm32", например "f407ve" или "l151c6-a").
/// Возвращает итоговые chip/chip_feature/cpu/target.
fn run_cascade_to(
    ast: &AST,
    target_suffix: &str,
) -> Result<HashMap<String, String>, Box<EvalAltResult>> {
    let vars: Vars = Rc::new(RefCell::new(HashMap::new()));
    let mut engine = Engine::new();

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

    let mut scope = Scope::new();
    engine.run_ast_with_scope(&mut scope, ast)?;

    Ok(vars.borrow().clone())
}

/// Полный список суффиксов из CHIPS в chip-select.rhai — читает тот же файл,
/// что и сам хук, не дублирует данные руками.
fn all_chip_suffixes() -> Vec<String> {
    let script = read_script();
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
            Ok(vars) => {
                let got = vars.get("chip_feature").map(String::as_str);
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
    let vars = run_cascade_to(&compile_script(), "l151c6").expect("каскад не должен падать");
    assert_eq!(
        vars.get("chip_feature").map(String::as_str),
        Some("stm32l151c6")
    );
    assert_eq!(vars.get("chip").map(String::as_str), Some("STM32L151C6"));
}

#[test]
fn cascade_reaches_the_longer_sibling_too() {
    let vars = run_cascade_to(&compile_script(), "l151c6-a").expect("каскад не должен падать");
    assert_eq!(
        vars.get("chip_feature").map(String::as_str),
        Some("stm32l151c6-a")
    );
    // Более точная цель probe-rs из PACKAGE_CHOICES, не общая "STM32L151C6".
    assert_eq!(vars.get("chip").map(String::as_str), Some("STM32L151C6TxA"));
}

#[test]
fn dual_core_suffix_uses_core_override_not_family_default() {
    // "h7" по family_table() даёт cortex-m7, но "-cm4" должен явно
    // перебивать это через core_override().
    let vars = run_cascade_to(&compile_script(), "h745zi-cm4").expect("каскад не должен падать");
    assert_eq!(vars.get("cpu").map(String::as_str), Some("cortex-m4"));
    assert_eq!(
        vars.get("target").map(String::as_str),
        Some("thumbv7em-none-eabihf")
    );
    assert_eq!(vars.get("chip").map(String::as_str), Some("STM32H745ZI"));
}
