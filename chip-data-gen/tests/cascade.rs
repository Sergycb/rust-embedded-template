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
type Files = Rc<RefCell<HashMap<String, Vec<String>>>>;
type Deleted = Rc<RefCell<Vec<String>>>;

/// Итог одного прогона: переменные (chip/chip_feature/cpu/target/write_size/
/// ota/bank_mode), файлы, записанные через `file::write` (memory.x), построчно,
/// и пути, снесённые через `file::delete` (cross/boot у чипов без OTA).
struct CascadeResult {
    vars: HashMap<String, String>,
    files: HashMap<String, Vec<String>>,
    deleted: Vec<String>,
}

/// Прогоняет уже скомпилированный chip-select.rhai один раз, ведя каскад к
/// `target_suffix` (суффикс без "stm32", например "f407ve" или "l151c6-a").
fn run_cascade_to(ast: &AST, target_suffix: &str) -> Result<CascadeResult, Box<EvalAltResult>> {
    let vars: Vars = Rc::new(RefCell::new(HashMap::new()));
    let files: Files = Rc::new(RefCell::new(HashMap::new()));
    let deleted: Deleted = Rc::new(RefCell::new(Vec::new()));
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
    engine.run_ast_with_scope(&mut scope, ast)?;

    Ok(CascadeResult {
        vars: vars.borrow().clone(),
        files: files.borrow().clone(),
        deleted: deleted.borrow().clone(),
    })
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
    let app = written(&result, "cross/app/memory.x");
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
    let boot = written(&result, "cross/boot/memory.x");
    assert!(
        boot.contains("FLASH             (rx)  : ORIGIN = 0x08000000, LENGTH = 64K"),
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
    let ast = compile_script();
    for suffix in [
        "f103rb", "f030r8", "f051r8", "f303vc", "l073rz", "l151cb", "l432kc", "l476rg", "g071rb",
        "g474re", "f407ve", "f411re", "f746zg", "h743zi",
    ] {
        let result = run_cascade_to(&ast, suffix).expect("каскад не должен падать");
        assert_eq!(
            result.vars.get("ota").map(String::as_str),
            Some("true"),
            "{suffix} остался без OTA: {:?}",
            result.files.get("cross/app/memory.x")
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
        "cross/app/memory.x",
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
        "cross/app/memory.x",
    );
    assert!(region_origin(&app, "CCMRAM").is_some(), "{app}");
    assert!(region_origin(&app, "OTP").is_some(), "{app}");

    // G474RE: CCMRAM_ICODE — второе окно того же блока, что CCMRAM_DCODE,
    // который уже внутри RAM. Объявить его рабочей памятью — значит дать
    // разместить данные дважды по одному физическому адресу, чего линкер не
    // поймает: выводим закомментированным.
    let app = written(
        &run_cascade_to(&ast, "g474re").expect("каскад не должен падать"),
        "cross/app/memory.x",
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
        "cross/app/memory.x",
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

    let app = written(&result, "cross/app/memory.x");
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
        app.contains("OTA (cross/boot) не подключён"),
        "причина должна остаться в самом memory.x:\n{app}"
    );
    // Каталог bootloader'а сносится самим хуком: [conditional] в
    // cargo-generate.toml переменных из хука не видит (проверено эмпирически).
    assert!(
        result.deleted.contains(&"cross/boot".to_string()),
        "cross/boot должен быть удалён, а удалено: {:?}",
        result.deleted
    );
    assert!(
        !result.files.contains_key("cross/boot/memory.x"),
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
    //      (`__bootloader_*` некому определить — cross/boot туда не входит).
    let ast = compile_script();
    let mut with_ota = 0;
    let mut without_ota = 0;
    let mut failures = Vec::new();
    for suffix in all_chip_suffixes() {
        let result = run_cascade_to(&ast, &suffix).expect("каскад не должен падать");
        let Some(app) = result.files.get("cross/app/memory.x") else {
            continue; // раскладка для чипа не считается — нечего проверять
        };
        let app = app.join("\n");
        let target_tests = result
            .files
            .get("cross/target-tests/memory.x")
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

        // Дамп паники и пользовательские данные, переживающие сброс, живут в
        // разных регионах: `panic-persist` пишет по голым адресам
        // `_panic_dump_start.._panic_dump_end` и в общем регионе затирал бы
        // секцию `.persist` молча — линкеру такое наложение не видно.
        let has_persist = region_origin(&app, "PERSIST").is_some();
        let has_panic = region_origin(&app, "PANIC").is_some();
        if has_persist != has_panic {
            fail("PERSIST и PANIC выводятся только вместе");
        }
        if has_panic != app.contains("_panic_dump_start = ORIGIN(PANIC);") {
            fail("регион PANIC есть, а символов panic-persist нет (или наоборот)");
        }
        if has_persist && region_origin(&app, "PERSIST") == region_origin(&app, "PANIC") {
            fail("PERSIST и PANIC начинаются с одного адреса — они наложены");
        }

        let Some(app_flash) = region_origin(&app, "FLASH") else {
            fail("в app/memory.x нет региона FLASH");
            continue;
        };

        if ota == Some("true") {
            with_ota += 1;
            let boot = written(&result, "cross/boot/memory.x");
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
        failures.is_empty(),
        "нарушены инварианты memory.x у {} чипов:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Списки чипов в `chip-select.rhai` собраны из чип-фич конкретной версии
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
    let manifest_path = script_path()
        .parent()
        .expect("chip-select.rhai лежит в корне репозитория")
        .join("cross")
        .join("Cargo.toml");
    let manifest =
        std::fs::read_to_string(&manifest_path).expect("не удалось прочитать cross/Cargo.toml");
    let declared = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("embassy-stm32"))
        .and_then(|line| line.split_once("version"))
        .and_then(|(_, rest)| rest.trim_start().strip_prefix('='))
        .and_then(|rest| rest.trim_start().strip_prefix('"'))
        .and_then(|rest| rest.split_once('"').map(|(version, _)| version.to_owned()))
        .expect("в cross/Cargo.toml не нашлась версия embassy-stm32");

    let expected = format!("// Источник: embassy-stm32 {declared} (cross/Cargo.toml)");
    assert!(
        read_script().contains(&expected),
        "chip-select.rhai собран не под embassy-stm32 {declared} — перегенерируйте списки:\n\
         cargo run --manifest-path chip-data-gen/Cargo.toml"
    );
}
