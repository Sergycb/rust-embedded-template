// pre-хук: каскадный посимвольный выбор STM32 (семейство -> линейка -> дальше
// по символу), подставляет chip_feature/chip/cpu/target/dual_core/bank_mode,
// write_size/ota и оба memory.x (см. MEMORY_LAYOUT ниже). Не init-хук — init
// выполняется ДО того, как cargo-generate резолвит `--define` в liquid-объект,
// поэтому `variable::is_set("chip_feature")` ниже не увидел бы значение с CLI
// (проверено эмпирически).
//
// Таблицы чипов лежат в chip-data/ (подключаются `import` ниже) и перегенерируются:
//   cargo run --manifest-path chip-data-gen/Cargo.toml
// при обновлении версии embassy-stm32 в crates-cross/Cargo.toml (сейчас "0.6.0",
// обычный semver-пин на crates.io, а не git-зависимость) или локально
// установленной версии probe-rs-tools.

fn family_table() {
    // Ключи отсортированы по убыванию длины — важно для семейств с общим
    // префиксом (wb/wba): "wba52cg" должен смэтчить "wba", а не короткий "wb".
    // Ядро/target зависят не от конкретного чипа, а от семейства — таблица
    // сверена с doctest-матрицей в начале embassy-stm32/Cargo.toml и с
    // документацией ST по ядрам Cortex-M используемых линеек.
    //
    // Двухъядерные чипы (STM32H745/747/755/757 — cm7/cm4, STM32WL54/55 —
    // cm4/cm0p) кодируют выбор ядра суффиксом прямо в чип-фиче
    // (`stm32h745zi-cm7`) — для них таблица ниже не используется вообще,
    // ядро и target берутся из core_override() по этому суффиксу.
    //
    // ОСОЗНАННОЕ ОГРАНИЧЕНИЕ (не забытая деталь, зафиксировано по решению
    // пользователя): для выбранного суффикса каскад выставляет переменную
    // `dual_core` (см. is_dual_core() и конец скрипта), которую crates-cross/bsp и
    // crates-cross/boot используют, чтобы вызвать `embassy_stm32::init_primary()`
    // вместо недоступного на dual-core `init()` — сгенерированный `cross`
    // собирается. Но код пишется только под ОДНО (то, что выбрано в
    // каскаде) ядро: второй образ шаблон не генерирует, взаимодействие с
    // фактическим вторым ядром не оркеструется. Полноценная
    // symmetric/asymmetric AMP-поддержка (второй независимый образ,
    // синхронизация через `SharedData` на реальном втором адресе,
    // раздельная OTA в crates-cross/boot) — отдельная архитектурная задача вне
    // шаблона, не сделана без отдельного запроса пользователя.
    //
    // ВАЖНО для STM32H745/747/755/757 (не относится к STM32WL54/55, см.
    // ниже): по умолчанию (option bytes BCM7=BCM4=1 у большинства ревизий)
    // ВТОРОЕ ядро тоже стартует при сбросе — не остаётся выключенным само по
    // себе — с адреса `BOOT_CM4_ADD0` (заводское значение — 0x08100000,
    // база Flash Bank2). Этот адрес попадает ровно на DFU-партицию из
    // MEMORY_LAYOUT ниже: после первого OTA-обновления там будет лежать
    // валидный образ прошивки CM7, и CM4 начнёт параллельно исполнять его
    // же на общей периферии/DMA — необслуживаемая гонка, не «второе ядро в
    // сбросе». Перед использованием проверьте/снимите BCM4 (или уведите
    // `BOOT_CM4_ADDx` на пустую область) через `probe-rs`/STM32CubeProgrammer
    // — шаблон это не делает. У STM32WL54/55 наоборот: CPU2 (CM0+) по
    // умолчанию не стартует, пока CPU1 явно не выставит `PWR_CR4.C2BOOT` —
    // там «остаётся в сбросе» после сгенерированной прошивки верно как есть.
    [
        ["wba", "cortex-m33", "thumbv8m.main-none-eabihf"],
        ["h5", "cortex-m33", "thumbv8m.main-none-eabihf"],
        ["l5", "cortex-m33", "thumbv8m.main-none-eabihf"],
        ["u3", "cortex-m33", "thumbv8m.main-none-eabihf"],
        ["u5", "cortex-m33", "thumbv8m.main-none-eabihf"],
        ["f3", "cortex-m4", "thumbv7em-none-eabihf"],
        ["f4", "cortex-m4", "thumbv7em-none-eabihf"],
        ["g4", "cortex-m4", "thumbv7em-none-eabihf"],
        ["l4", "cortex-m4", "thumbv7em-none-eabihf"],
        ["wb", "cortex-m4", "thumbv7em-none-eabihf"],
        ["wl", "cortex-m4", "thumbv7em-none-eabihf"],
        ["f7", "cortex-m7", "thumbv7em-none-eabihf"],
        ["h7", "cortex-m7", "thumbv7em-none-eabihf"],
        ["f1", "cortex-m3", "thumbv7m-none-eabi"],
        ["f2", "cortex-m3", "thumbv7m-none-eabi"],
        ["l1", "cortex-m3", "thumbv7m-none-eabi"],
        ["c0", "cortex-m0plus", "thumbv6m-none-eabi"],
        ["f0", "cortex-m0", "thumbv6m-none-eabi"],
        ["g0", "cortex-m0plus", "thumbv6m-none-eabi"],
        ["l0", "cortex-m0plus", "thumbv6m-none-eabi"],
        ["u0", "cortex-m0plus", "thumbv6m-none-eabi"],
    ]
}

fn core_override(marker) {
    // Суффикс после дефиса, когда он обозначает КОНКРЕТНОЕ ядро (двухъядерные
    // чипы: STM32H745/747/755/757 — cm7/cm4, STM32WL54/55 — cm4/cm0p) —
    // тогда семейство в family_table() даёт не то ядро (например у "h7" по
    // умолчанию cortex-m7, но "-cm4" явно просит второе ядро). Другие
    // суффиксы после дефиса (силиконовая градация вроде "l151c6-a") ядро не
    // переопределяют — для них возвращается "не найдено" (`()`), и
    // используется обычная таблица по семейству.
    switch marker {
        "cm0" => ["cortex-m0", "thumbv6m-none-eabi"],
        "cm0p" => ["cortex-m0plus", "thumbv6m-none-eabi"],
        "cm3" => ["cortex-m3", "thumbv7m-none-eabi"],
        "cm4" => ["cortex-m4", "thumbv7em-none-eabihf"],
        "cm7" => ["cortex-m7", "thumbv7em-none-eabihf"],
        "cm23" => ["cortex-m23", "thumbv8m.base-none-eabi"],
        "cm33" => ["cortex-m33", "thumbv8m.main-none-eabihf"],
        _ => (),
    }
}

fn cpu_and_target(suffix) {
    let dash = suffix.index_of("-");
    if dash >= 0 {
        let over = core_override(suffix.sub_string(dash + 1));
        if over != () {
            return over;
        }
    }
    for row in family_table() {
        if suffix.starts_with(row[0]) {
            return [row[1], row[2]];
        }
    }
    abort("chip-select: неизвестное семейство STM32 для \"" + suffix
        + "\" — добавьте его в family_table() в chip-select.rhai");
}

/// Наименьший раздел приложения, в который граф задач ещё имеет смысл класть.
///
/// Порог эмпирический, и обе точки замерены на реальной сборке шаблона:
/// STM32L011F4 (16 KiB, Cortex-M0+) не собирается вовсе — линкер не досчитывает
/// двух сотен байт; STM32C011D6 (32 KiB, то же ядро) занимает 12.6 KiB из 32,
/// то есть держит запас в шестьдесят процентов под ваш код. Сам `supervisor`
/// стоит порядка полутора килобайт на Cortex-M4 и до пяти на Cortex-M0+ —
/// разброс из-за того, что на Armv6-M крупнее и код, и арифметика времени.
///
/// Порог намеренно консервативный: лучше не дать каркас там, где он влез бы
/// впритык, чем отдать проект, который перестанет собираться от первой же
/// пользовательской задачи.
const GRAPH_MIN_PARTITION = 32768;

/// Ниже какого размера раздела `ACTIVE` OTA-схему предлагать нельзя.
///
/// Дело не в самой схеме — её раскладка сходится и на 8 KiB, — а в том, что в
/// такой раздел не помещается даже пустая прошивка шаблона. Замерено сборкой:
/// `f030c8` (15 KiB под приложение) переполняет раздел на 848 байт, `c051c8`
/// (14 KiB) — на 1904, `l051c8` (15.9 KiB) — на 272. Проект генерировался,
/// а собрать его было нельзя вовсе — то есть OTA-схема отбирала flash у
/// прошивки, которая без неё прекрасно помещается в тот же чип целиком.
///
/// Порог стоит между двумя реальными группами чипов: у 160 из них под
/// приложение выходит 16256 байт и меньше, а следующий по величине раздел —
/// сразу 40 KiB. Любое значение внутри этого промежутка отсекает ровно
/// неработающие конфигурации, поэтому взято круглое.
const OTA_MIN_ACTIVE = 24576;

/// Меньше этого объёма EEPROM под настройки не годится: `sequential-storage`
/// нужны две страницы, а страница обёртки `adapters::eeprom::Eeprom` — 128
/// байт, то есть минимум две страницы (`Eeprom::PAGE`). У STM32L010F4/K4 EEPROM всего 128 байт — им
/// настройки предлагаются, как всем остальным, во флеше.
const EEPROM_MIN = 256;

/// Размер из таблицы раскладки в байтах: `"128K"`, `"1M"`, `"5376"`.
///
/// Свой разбор, а не готовая функция: в rhai её нет, а формат здесь пишем мы
/// сами — те же строки уходят в `memory.x`, где их читает линкер.
fn size_in_bytes(text) {
    // `trim()` в rhai мутирует строку на месте и возвращает (), поэтому копия
    // и отдельный вызов, а не `let raw = text.trim();`.
    let raw = text;
    raw.trim();
    let last = raw.sub_string(raw.len - 1, 1);
    if last == "K" || last == "k" {
        return parse_int(raw.sub_string(0, raw.len - 1)) * 1024;
    }
    if last == "M" || last == "m" {
        return parse_int(raw.sub_string(0, raw.len - 1)) * 1024 * 1024;
    }
    parse_int(raw)
}

/// Где лежат настройки (`config_store`): `"flash"` — раздел во внутреннем
/// флеше, `"eeprom"` — внутренняя EEPROM (объёмом не меньше `EEPROM_MIN`,
/// регионы `EEPROM`/`EEPROM_BANK_n`; закомментированные строки — окна внешних
/// шин, алиасы — начинаются с `/*`, а не с имени), `"external"` — внешняя
/// флеш. Ни внешней флеши, ни EEPROM раскладка flash не нужна.
fn config_store(m, wants_config, wants_external, eeprom_min) {
    let eeprom_size = 0;
    if m.contains("extra_regions") {
        for line in m["extra_regions"] {
            if line.starts_with("    EEPROM") {
                eeprom_size += size_in_bytes(line.split("LENGTH = ")[1]);
            }
        }
    }
    if wants_external {
        "external"
    } else if wants_config && eeprom_size >= eeprom_min {
        "eeprom"
    } else {
        "flash"
    }
}

/// Что получится из ответов на чипе с раскладкой `m`: помещается ли OTA,
/// раздел настроек и граф. Чистая функция — без `print` и без `variable::set`:
/// её зовёт и сам хук (выбранный чип), и фильтр списка чипов (все чипы
/// сразу), и решение обязано быть одно.
///
/// `wants` — ответы пользователя (`ota`, `ext_ota`, `config`, `external`,
/// `graph`), `limits` — пороги `OTA_MIN_ACTIVE`, `GRAPH_MIN_PARTITION` и `EEPROM_MIN`:
/// функция не видит глобальных констант скрипта.
fn plan(m, erase_zero, wants, limits) {
    let store = config_store(m, wants.config, wants.external, limits.eeprom_min);

    // Раздел настроек решается первым: он меняет всю OTA-схему — в хвосте
    // отрезает flash, а в резерве перед ACTIVE (F2/F4/F7) теснит bootloader и
    // STATE. Под это в таблице отдельный набор цифр с префиксом `config_` — см.
    // ConfigPartition в chip-data-gen.
    // `!erase_zero` — то же ограничение, что и у OTA, только у другой
    // библиотеки: `sequential-storage` считает стёртым состоянием 0xFF
    // (`item.rs`: заголовок ищется как «все байты 0xFF», а состояние страницы —
    // по нулевым маркерам), и настраиваемого значения у неё нет. На флеше,
    // стирающемся в ноль, свежая страница выглядит «уже закрытой», и хранилище
    // молча не работает. Раздавать такое нельзя.
    // Раздел во внутреннем флеше — только когда настройки хранятся там.
    let in_flash = wants.config && store == "flash" && m["config"] == "true" && !erase_zero;
    let config = wants.external || (wants.config && store == "eeprom") || in_flash;

    // Дальше партиции берутся по одному из наборов ключей: `key` — раскладка
    // без OTA (весь flash или он без раздела настроек в хвосте), `ota_key` —
    // раскладка с OTA. Внутренняя OTA — те же наборы (`` или `config_`),
    // внешняя — свои (`ext_` или `ext_cfg_`, настройки во внутреннем флеше).
    let key = if in_flash { "config_" } else { "" };
    let ext_key = if in_flash { "ext_cfg_" } else { "ext_" };
    let ota_key = if wants.ext_ota { ext_key } else { key };

    // Схема нужна только если её и хотят, и она помещается. «Помещается» — это
    // две проверки: раскладка вообще сходится (цифры посчитаны заранее) и в
    // разделе `ACTIVE` остаётся место под саму прошивку.
    let ota_fits = m[ota_key + "ota"] == "true"
        && size_in_bytes(m[ota_key + "active_length"]) >= limits.ota_min;
    let ext_fits = m[ext_key + "ota"] == "true"
        && size_in_bytes(m[ext_key + "active_length"]) >= limits.ota_min;
    let ota = wants.ota && ota_fits;
    let ext_ota = ota && wants.ext_ota;

    // Где лежит раздел настроек. Без OTA — в хвосте flash (`config_*`): в
    // начале стоит таблица векторов приложения. С OTA — там, где его положил
    // chip-data-gen (`config_ota_settings_*`): в мелких секторах перед ACTIVE,
    // если так ACTIVE больше (F2/F4/F7), иначе тоже в хвосте. Страница — сектор
    // под разделом (`adapters::flash::Paged` в bsp::config).
    let settings_key = if ext_ota {
        "ext_cfg_settings_"
    } else if ota {
        "config_ota_settings_"
    } else {
        "config_"
    };

    // Граф задач меряется по разделу, в который линкуется приложение: при OTA
    // это ACTIVE, без неё — весь flash за вычетом того, что забрали настройки.
    // Ключ считается один раз и используется и для проверки, и для
    // сообщения: с двумя копиями выбора хук однажды сообщил бы размер,
    // который не измерял.
    let app_length_key = if ota { ota_key + "active_length" } else { key + "flash_length" };
    let graph = wants.graph && size_in_bytes(m[app_length_key]) >= limits.graph_min;

    #{
        store: store,
        in_flash: in_flash,
        config: config,
        key: key,
        ext_key: ext_key,
        ota_key: ota_key,
        ota_fits: ota_fits,
        ext_fits: ext_fits,
        ota: ota,
        ext_ota: ext_ota,
        settings_key: settings_key,
        app_length_key: app_length_key,
        graph: graph,
    }
}

/// Выполняются ли ответы на чипе: каждое «хочу» из `need` должно получиться.
/// План считается по `wants` (все ответы, с умолчаниями), а проверяется по
/// `need` (только то, о чём спросили): так умолчание «да» для вопроса, который
/// не задавался, чипов не отсекает.
fn fits_answers(m, erase_zero, wants, need, limits) {
    let p = plan(m, erase_zero, wants, limits);
    (!need.ota || p.ota) && (!need.ext_ota || p.ext_ota)
        && (!need.config || p.config) && (!need.graph || p.graph)
}

fn is_dual_core(suffix) {
    // Тот же признак, что различает ветки в cpu_and_target(): суффикс после
    // дефиса — это конкретное ядро (core_override() узнаёт его), а не
    // силиконовая градация вроде "-a"/"-x" (для неё core_override() вернёт
    // "не найдено", и суффикс не dual-core).
    let dash = suffix.index_of("-");
    if dash < 0 {
        return false;
    }
    core_override(suffix.sub_string(dash + 1)) != ()
}

let suffix = "";

// Массив нужен и ветке `--define chip_feature=...` (для валидации), и
// каскаду ниже — поэтому объявлен на верхнем уровне скрипта, а не внутри
// одной из веток.
import "chip-data/chips" as chips_data;
const CHIPS = chips_data::CHIPS;

if CHIPS.len() == 0 {
    abort("chip-select: список чипов пуст — запустите "
        + "`cargo run --manifest-path chip-data-gen/Cargo.toml` перед генерацией");
}

// Для чипов с суффиксом-градацией, не обозначающим ядро (например `l151c6-a`
// — автомобильный температурный диапазон STM32L1xx, не путать с `-cm7`/`-cm4`
// у двухъядерных, те решает core_override()), базовая цель probe-rs не
// всегда самая точная — у пробы отдельно бывают корпусировки с другим
// объёмом RAM (`STM32L151C6TxA`/`STM32L151C6UxA` против `STM32L151C6`).
// Ключ — суффикс чипа, значение — все такие цели (длиннее базовой);
// пусто/нет ключа — точнее базовой цели ничего нет.
import "chip-data/packages" as packages_data;
const PACKAGE_CHOICES = packages_data::PACKAGE_CHOICES;

// Чипы, у которых stm32-metapac несёт несколько карт памяти (одно- и
// двухбанковую): для них build.rs самого embassy-stm32 ТРЕБУЕТ фичу
// "single-bank"/"dual-bank" и без неё паникует ("Chip supports single and dual
// bank configuration. No Cargo feature to select one is enabled"), то есть
// проект не собирается вовсе. Для остальных чипов такой фичи нет — хук
// подставляет пустую строку, и {% if %} в crates-cross/Cargo.toml её не выводит.
import "chip-data/bank_mode" as bank_mode_data;
const BANK_MODE = bank_mode_data::BANK_MODE;

// Чипы, у которых раскладка одобанковая, а с завода флеш двухбанковый (G0B/G0C,
// G4 cat3, L4+, L5): режим переключает сама прошивка (crates-cross/bsp/src/banks.rs,
// docs/flash.md). Значение — вариант регистров option bytes.
import "chip-data/bank_switch" as bank_switch_data;
const BANK_SWITCH = bank_switch_data::BANK_SWITCH;

// Как у чипа зовётся блок независимого сторожевого таймера. Нужно затем, что
// `bsp` объявляет поле типа `wdg::Iwdg<peripherals::IWDG…>`, а имя типа в
// embassy-stm32 — ровно то, что стоит в метаданных: у подавляющего
// большинства чипов `IWDG`, у H7 `IWDG1`, у двухъядерных H7 ещё и `IWDG2`
// (первый принадлежит ядру CM7, второй — CM4; метаданные обоих ядер
// перечисляют оба, поэтому выбор делается по уже выбранному каскадом
// суффиксу `-cm4`). Таблица перечисляет только исключения — остальным хук
// подставляет "IWDG", и полная таблица на 1479 чипов повторяла бы одно слово
// полутора тысячами строк.
//
// Значение приходит из метаданных, а НЕ выводится из имени чипа: ровно на
// такой эвристике уже ошибся признак erase_zero (см. комментарий к нему
// ниже), и повторять это не надо.
import "chip-data/watchdog" as watchdog_data;
const WATCHDOG = watchdog_data::WATCHDOG;

// Чипы с option byte CPU_FREQ_BOOST (STM32H723/725/730/733/735, RM0468): без
// него потолок ядра на VOS0 — 520 МГц, с ним — 550 МГц. Признак — блок PWR
// версии h7rm0468 в метаданных, по нему же embassy-stm32 выбирает предел
// частоты. Таким чипам bsp получает `ensure_cpu_freq_boost()` (docs/clocks.md).
import "chip-data/cpu_freq_boost" as boost_data;
const CPU_FREQ_BOOST = boost_data::CPU_FREQ_BOOST;

// Интерфейсы связи, выведенные в корпусе чипа (`usart`, `fdcan`, `bxcan`,
// `eth`, `usb`, `otg`) — по ним проверяется ответ об интерфейсе в конце хука.
import "chip-data/interfaces" as interfaces_data;
const INTERFACES = interfaces_data::INTERFACES;
// Имя блока каждого интерфейса (`USART1`, `FDCAN1`, `USB_OTG_HS`...) — для групп
// `resources.rs`: по метке его не угадать, у части чипов нет и `USART2`, и
// `USB_OTG_FS`.
import "chip-data/instances" as instances_data;
const INSTANCES = instances_data::INSTANCES;

// Аппаратный RNG: [прерывание, клок, источник] — по ним bsp заводит
// `bsp::rng` и даёт блоку клок в конфиге HAL (docs/modules/bsp-rng.md). Чипа
// нет в таблице — RNG у него нет или источник клока не проверен.
import "chip-data/rng" as rng_data;
const RNG = rng_data::RNG;

// Ответ на вопрос про OTA из [placeholders] (или `--define ota=no`). Дальше по
// коду `ota` перезаписывается уже вычисленной строкой `"true"`/`"false"`: на
// неё смотрят Liquid-условия в шаблоне.
//
// «Да» здесь означает лишь «не возражаю»: помещается ли схема в flash, решает
// раскладка ниже. А вот «нет» — окончательное, и проверять нечего.
//
// Где лежат `DFU` и `STATE` (`ota_store`, на него смотрят boot и bsp::flash):
//   `internal` — во внутреннем flash, рядом с `ACTIVE`;
//   `external` — на внешней флеши: внутри остаются bootloader и `ACTIVE`,
//                который забирает почти весь flash; драйвер микросхемы
//                подставляет проект (в шаблоне — заглушка).
// Вопрос задаётся до каскада и сам сужает список чипов (см. `allowed` ниже);
// для чипа, заданного напрямую, невозможный вариант отсекается с подсказкой.
//
// Незнакомый ответ (опечатка в `--define`) — ошибка, а не «без OTA»: молча
// снятый bootloader заметили бы только на первом обновлении.
let wants_ota = true;
let wants_ext_ota = false;
if variable::is_set("ota") {
    let answer = variable::get("ota");
    if answer != "internal" && answer != "external" && answer != "no" {
        throw "ota: ожидается internal, external или no, а не «" + answer + "»";
    }
    wants_ext_ota = answer == "external";
    wants_ota = answer != "no";
}

if !wants_ota {
    print("OTA отключён по вашему выбору: один образ на весь flash, crates-cross/boot в проект не входит");
}

// Раздел под настройки — вопрос того же рода, что и про OTA, и с той же
// механикой ответа. «Да» здесь тоже означает «не возражаю»: хватит ли под него
// места, решает раскладка ниже.
//
// Стоит он дороже, чем кажется: две страницы стирания. С OTA chip-data-gen
// кладёт их в мелкие сектора перед ACTIVE, где это выгоднее (на F4 — 2×16 KiB),
// но без OTA это две последние страницы flash по максимальному сектору чипа: на
// F4/F7/H7 — 256 KiB. Поэтому по умолчанию «нет», а цена печатается ниже — до
// того, как пользователь увидит собранный проект.
//
// Где лежат настройки (`config_store`, на него смотрит bsp::config):
//   "flash"    — раздел во внутреннем флеше (как описано выше);
//   "eeprom"   — внутренняя EEPROM, если она у чипа есть (L0/L1): флеш не
//                тратится вовсе, и это единственное место под настройки на
//                чипах, чей флеш стирается в ноль;
//   "external" — внешняя флеш: ответ `external`; шаблон кладёт заглушку
//                (`adapters::flash::Unconnected`), драйвер подставляете вы;
//   "none"     — настроек нет.
// Ответ `internal` выбирает между первыми двумя сам, `external` — третий,
// `no` — четвёртый. Незнакомый ответ — ошибка, как у `ota`.
let wants_config = false;
let wants_external = false;
if variable::is_set("config") {
    let answer = variable::get("config");
    if answer != "internal" && answer != "external" && answer != "no" {
        throw "config: ожидается internal, external или no, а не «" + answer + "»";
    }
    wants_external = answer == "external";
    wants_config = answer != "no";
}

// Граф задач. «Да» и здесь означает «не возражаю»: помещается ли он, решает
// размер раздела, в который линкуется приложение (см. GRAPH_MIN_PARTITION).
let wants_graph = true;
if variable::is_set("graph") {
    let answer = variable::get("graph");
    wants_graph = answer == "yes" || answer == "true";
}
if !wants_graph {
    print("Граф задач отключён по вашему выбору: main заканчивается после инициализации");
}

// Пороги для `plan()`: функция глобальных констант не видит.
const LIMITS = #{ ota_min: OTA_MIN_ACTIVE, graph_min: GRAPH_MIN_PARTITION, eeprom_min: EEPROM_MIN };
const WANTS = #{
    ota: wants_ota,
    ext_ota: wants_ext_ota,
    config: wants_config,
    external: wants_external,
    graph: wants_graph,
};

// Что из ответов реально задано: в генерации через cargo-generate заданы все
// вопросы (у них есть умолчания), а в прогоне без них «не спрашивали» не
// должно отсекать чипы.
const NEED = #{
    ota: variable::is_set("ota") && wants_ota,
    ext_ota: variable::is_set("ota") && wants_ext_ota,
    config: variable::is_set("config") && wants_config,
    graph: variable::is_set("graph") && wants_graph,
};

// Блоки чипа (метки chip-data/interfaces.rhai), на которых работает
// интерфейс: CAN — любой из двух контроллеров, имя блока в документации и
// справочнике выбирает `can_kind`.
const INTERFACE_BLOCKS = #{
    "uart": ["usart"],
    "can": ["fdcan", "bxcan"],
    "tcp": ["eth"],
    "udp": ["eth"],
    "usb": ["usb", "otg"],
};

// Ответы отсекают чипы ещё до каскада: сетевой стек просит чип с Ethernet,
// OTA — чип, где схема помещается, граф — где он влезает в раздел
// приложения. Каскад по символам тогда предлагает только то, с чем ответы
// выполнимы, а не отказывает в конце, когда выбор уже сделан. Чип, заданный
// напрямую (`--define chip_feature=...`), не фильтруется: там остаются
// прежние отказы с объяснением.
const WANTED_INTERFACE = if variable::is_set("interface") { variable::get("interface") } else { "none" };
let allowed = CHIPS;
if !variable::is_set("chip_feature")
    && (NEED.ota || NEED.config || NEED.graph || WANTED_INTERFACE != "none")
{
    let layouts = #{};
    for chip in CHIPS {
        let family = chip.sub_string(0, 2);
        if !layouts.contains(family) {
            import "chip-data/layout/" + family as family_layout;
            layouts[family] = family_layout::MEMORY_LAYOUT;
        }
    }
    allowed = CHIPS.filter(|chip| {
        if WANTED_INTERFACE != "none" && INTERFACE_BLOCKS.contains(WANTED_INTERFACE) {
            let have = INTERFACES[chip];
            let any_block = false;
            for tag in INTERFACE_BLOCKS[WANTED_INTERFACE] {
                if have.contains(tag) { any_block = true; }
            }
            if !any_block { return false; }
        }
        let m = layouts[chip.sub_string(0, 2)][chip];
        fits_answers(m, m["erase_zero"] == "true", WANTS, NEED, LIMITS)
    });
    if allowed.len() == 0 {
        throw "ни один из " + CHIPS.len() + " чипов не подходит под ответы (interface=" + WANTED_INTERFACE
            + ", ota=" + (if wants_ext_ota { "external" } else if wants_ota { "internal" } else { "no" })
            + ", config=" + (if wants_external { "external" } else if wants_config { "internal" } else { "no" })
            + ", graph=" + (if wants_graph { "yes" } else { "no" })
            + ") — смягчите ответы: чип поменьше берёт ota=no и graph=no";
    }
    if allowed.len() < CHIPS.len() {
        print("Под ваши ответы подходит " + allowed.len() + " из " + CHIPS.len() + " чипов; остальные не показываются."
            + " Нужен чип поменьше — ответьте ota=no / graph=no (или interface=none).");
    }
}

if variable::is_set("chip_feature") {
    // Значение уже задано через `--define chip_feature=...` — каскад не
    // нужен, chip/cpu/target выводятся из него напрямую.
    let cf = variable::get("chip_feature");
    if !cf.starts_with("stm32") || cf.len() <= 5 {
        abort("chip-select: chip_feature должен быть вида \"stm32xxxxxx\", получено \""
            + cf + "\"");
    }
    suffix = cf.sub_string(5);
    if !CHIPS.contains(suffix) {
        abort("chip-select: chip_feature \"" + cf + "\" не входит в список валидных "
            + "чипов — проверьте опечатку, либо, если это новый чип, перегенерируйте "
            + "список: `cargo run --manifest-path chip-data-gen/Cargo.toml`");
    }
} else {
    let prefix = "";
    loop {
        let candidates = allowed.filter(|c| c.starts_with(prefix));

        if candidates.len() == 1 {
            prefix = candidates[0];
            break;
        }

        let choices = [];
        if candidates.contains(prefix) {
            // prefix сам по себе уже валидный чип, но у него есть и более
            // длинные продолжения — явный вариант "остановиться здесь".
            choices.push(prefix);
        }

        let next_chars = [];
        for c in candidates {
            if c.len() > prefix.len() {
                let nc = c.sub_string(prefix.len(), 1);
                if !next_chars.contains(nc) {
                    next_chars.push(nc);
                }
            }
        }
        next_chars.sort();
        for nc in next_chars {
            choices.push(prefix + nc);
        }

        if choices.len() == 1 {
            // Единственный вариант продолжения — нечего выбирать, не
            // переспрашивать (возможно только когда prefix ещё не сам
            // валидный чип, иначе он был бы вторым вариантом в choices).
            prefix = choices[0];
        } else {
            let label = "Выберите STM32 (" + ("stm32" + prefix).to_upper() + "...)";
            let pick = variable::prompt(label, choices[0], choices);
            if pick == prefix {
                // Выбран сам prefix — вариант "остановиться здесь". Без
                // этой проверки prefix не менялся бы, choices на следующей
                // итерации получались бы теми же, и цикл зависал бы
                // навсегда на одном и том же вопросе (актуально для
                // чипов, чей суффикс — строгий префикс другого, например
                // "l151c6" при живом "l151c6-a").
                break;
            }
            prefix = pick;
        }
    }
    suffix = prefix;
}

let family = cpu_and_target(suffix);
let dual_core = is_dual_core(suffix);

variable::set("chip_feature", "stm32" + suffix);
// `chip`/`cpu`/`target` можно переопределить отдельно через `--define` (не
// только через `chip_feature`).
if !variable::is_set("chip") {
    // Двухъядерные чипы (STM32H745/747/755/757, STM32WL54/55) кодируют
    // выбор ядра прямо в фиче embassy-stm32 (`h745zi-cm7` / `h745zi-cm4`),
    // но probe-rs об этом суффиксе не знает — у него одна цель на весь чип
    // (`STM32H745ZI`), ядро выбирается отдельно при прошивке/отладке, не
    // именем цели.
    let dash = suffix.index_of("-");
    let chip_id = if dash < 0 { suffix } else { suffix.sub_string(0, dash) };
    let default_chip = "STM32" + chip_id.to_upper();

    // Силиконовые градации (не ядра — те выше) могут иметь более точную,
    // чем default_chip, цель probe-rs (PACKAGE_CHOICES) — например у
    // `l151c6-a` (10 KiB RAM у общей "STM32L151C6") находится настоящая
    // automotive-цель с 16 KiB. Кандидатов может быть несколько (разная
    // корпусировка — `TxA`/`UxA`/...), но между ними не выбираем: они
    // отличаются только буквой корпуса, а не памятью/ядром/чем-либо, что
    // видит probe-rs (проверено по `probe-rs chip info` для всех случаев
    // при разработке) — берём первый (список отсортирован, стабильно).
    if PACKAGE_CHOICES.contains(suffix) {
        default_chip = PACKAGE_CHOICES[suffix][0];
    }

    variable::set("chip", default_chip);
}
if !variable::is_set("cpu") {
    variable::set("cpu", family[0]);
}
if !variable::is_set("target") {
    variable::set("target", family[1]);
}
// Читается из crates-cross/bsp/Cargo.toml и crates-cross/boot/Cargo.toml (условно
// включает фичу "dual-core" в default) — см. Board::new()/main() там же.
// Без переопределения через --define (в отличие от chip/cpu/target/
// write_size): значение полностью выводится из chip_feature, легитимной
// причины расходиться с ним нет.
variable::set("dual_core", if dual_core { "true" } else { "false" });

// Банковая схема (см. BANK_MODE выше) — фича embassy-stm32, без которой
// многоконфигурационные чипы не собираются вовсе. Без переопределения через
// --define, как и dual_core: значение целиком выводится из chip_feature.
variable::set("bank_mode", if BANK_MODE.contains(suffix) { BANK_MODE[suffix] } else { "" });

// Переключение в одобанковый режим (см. BANK_SWITCH выше): вариант регистров или пустая
// строка. Без переопределения через --define, как и bank_mode.
// Значение таблицы — «вариант:бит» (`g4:22`): бит FLASH_OPTR двухбанкового режима у L4+ и
// L5 зависит от объёма (см. `bank_switch_bits` в chip-data-gen).
let bank_switch = "";
let dual_bank_bit = "";
if BANK_SWITCH.contains(suffix) {
    let parts = BANK_SWITCH[suffix].split(":");
    bank_switch = parts[0];
    dual_bank_bit = parts[1];
}
variable::set("bank_switch", bank_switch);
variable::set("dual_bank_bit", dual_bank_bit);
if bank_switch == "" {
    file::delete("crates-cross/bsp/src/banks.rs");
}

// Имя блока сторожевого таймера (см. WATCHDOG выше) — подставляется в тип
// поля `Board::watchdog` в bsp и в графовый блок `watchdog:` в app. Без
// переопределения через --define, как dual_core и bank_mode: значение целиком
// выводится из chip_feature.
variable::set("watchdog_peripheral", if WATCHDOG.contains(suffix) { WATCHDOG[suffix] } else { "IWDG" });

// Option byte CPU_FREQ_BOOST (см. CPU_FREQ_BOOST выше): включает в bsp модуль
// `clocks` с `ensure_cpu_freq_boost()`. Без переопределения через --define:
// значение целиком выводится из chip_feature.
variable::set("cpu_freq_boost", if CPU_FREQ_BOOST.contains(suffix) { "true" } else { "false" });
if !CPU_FREQ_BOOST.contains(suffix) {
    file::delete("crates-cross/bsp/src/clocks.rs");
}

// RNG (см. RNG выше). Без переопределения через --define: значение целиком
// выводится из chip_feature.
let rng = if RNG.contains(suffix) { RNG[suffix] } else { ["", "", ""] };
variable::set("rng_irq", rng[0]);
variable::set("rng_clock", rng[1]);
variable::set("rng_source", rng[2]);
// Файлы модуля удаляются ниже, после проверки interface: RNG заводится только
// под tcp/udp (сид стека `embassy-net`) и только там, где он есть у чипа.

// Готовые адреса/размеры для memory.x. Строится из метаданных stm32-metapac
// (реальные границы секторов чипа), не гадание — см. chip-data-gen/src/main.rs,
// compute_memory_layout(). У каждой записи есть "ota":
//   "true"  — OTA-схема embassy-boot помещается: есть BOOTLOADER/ACTIVE/DFU/
//             BOOTLOADER_STATE, crates-cross/boot собирается и прошивается;
//   "false" — не помещается (слишком мелкий flash или слишком крупные сектора,
//             причина в "note"): один образ на весь flash, crates-cross/boot из
//             сборки исключается.
// "write_size" — const generic BootLoader::prepare::<_,_,_,N>, размер буфера
// подкачки (НЕ PAGE_SIZE: тот embassy-boot вычисляет сам из ERASE_SIZE флеша).
// "page_size" — физическая страница чипа; страница обмена (PAGE_SIZE embassy-boot)
// лежит в "swap_page_size" и бывает крупнее — "swap_paged" = "true": тогда ACTIVE
// и DFU прошивка оборачивает в Paged ради малого журнала STATE.
// Имя плейсхолдера осталось прежним ради совместимости с проектами, которые
// задают его через --define.
//
// Таблица разложена по файлам семейств — первые два символа суффикса, тот же
// ключ, что `layout_family` в chip-data-gen: целиком она ~89 тысяч строк, а
// нужна одна запись.
import "chip-data/layout/" + suffix.sub_string(0, 2) as layout_data;
const MEMORY_LAYOUT = layout_data::MEMORY_LAYOUT;
// Запись есть у каждого чипа из CHIPS: без неё chip-data-gen падает, не
// записав каталог. Нет записи — значит хук и chip-data/ разошлись (другое
// правило семейства у `layout_family`, недоперегенерированный каталог).
if !MEMORY_LAYOUT.contains(suffix) {
    abort("chip-select: в chip-data/layout/ нет раскладки для \"" + suffix + "\" — "
        + "перегенерируйте: `cargo run --manifest-path chip-data-gen/Cargo.toml`");
}

// Чем читается СТЁРТАЯ страница флеша. У подавляющего большинства семейств
// это 0xFF, а у 188 чипов из 1479 — 0x00 (их NVM стирается в ноль: страница
// стирается записью с флагами ERASE|PROG, см. flash/l.rs в embassy-stm32).
//
// Знать это обязан не только код, но и `embassy-boot`: он отличает стёртое
// состояние от записанного, сравнивая байты с зашитой константой
// (`STATE_ERASE_VALUE`), и переключается фичей `flash-erase-zero`. Ошибись мы
// здесь — обмен разделов читал бы признак «стёрто» наоборот, то есть OTA не
// работала бы, причём молча. Вторая жертва — раздел настроек:
// `sequential-storage` ищет заголовок записи как «все байты 0xFF» и на таком
// флеше видит свежую страницу закрытой.
//
// Значение приходит из метаданных (`erase_value` во `FlashSettings`, поле
// `erase_zero` в MEMORY_LAYOUT), а НЕ выводится из имени чипа. Раньше здесь
// стояло `starts_with("l0") || starts_with("l1")` — и мимо этой эвристики
// проходили STM32WB10CC и STM32WB15CC: у них `erase_value: 0` при совсем
// другом префиксе, оба есть в каскаде, и оба получали неработающие OTA и
// раздел настроек. Эвристика по имени рядом с метаданными — это второй
// источник правды, и он ошибся ровно так, как такие источники и ошибаются.
let erase_zero = MEMORY_LAYOUT[suffix]["erase_zero"] == "true";
variable::set("erase_zero", if erase_zero { "true" } else { "false" });

// Где лежат настройки (`config_store`) — одним решением для всех трёх веток
// ниже (см. `config_store()` выше).
let store = config_store(MEMORY_LAYOUT[suffix], wants_config, wants_external, EEPROM_MIN);

// write_size, оба memory.x и признак ota — вместе, одним источником: если
// пользователь уже задал write_size вручную через `--define`, авточипу
// доверять нельзя (адреса в memory.x привязаны именно к вычисленному
// write_size), оставляем всё как есть — ручное заполнение, как раньше.
// Тогда же остаётся и ota = "true": bootloader из проекта не исчезает,
// раскладку под него пользователь пишет сам.
if variable::is_set("write_size") {
    variable::set("ota", if wants_ota { "true" } else { "false" });
    variable::set("ota_store", if wants_ext_ota { "external" } else { "internal" });
    // Разкладку здесь пишет человек: DFU через регион банка 2 включает он сам.
    variable::set("dfu_bank", "false");
    if !wants_ota {
        file::delete("crates-cross/boot");
        // Релизу по тегу без OTA нечего выкладывать: `app.bin` делается
        // только там, где есть раздел DFU. Тело workflow и так под
        // Liquid-условием, но пустой файл в .github/workflows сбивал бы с
        // толку — GitHub показывал бы его в списке рабочих процессов.
        file::delete(".github/workflows/release.yml");
    }
    // Раздел настроек во флеше живёт по границам из memory.x, а его тут пишет
    // пользователь — посчитать за него, где начинается CONFIG, нельзя. Внешней
    // флеши и EEPROM раскладка не нужна, их слот работает и здесь.
    if wants_config && store != "flash" {
        variable::set("config", "true");
        variable::set("config_store", store);
    } else {
        variable::set("config", "false");
        variable::set("config_store", "none");
        if wants_config {
            print("Раздел настроек не выделен: memory.x вы заполняете сами (задан write_size)");
            print("  добавьте регион CONFIG и символы __config_start/__config_end вручную");
        }
        file::delete("crates-cross/bsp/src/config.rs");
    }
    // Общий объект `Flash` в bsp нужен OTA во внутреннем flash и настройкам во
    // флеше или EEPROM; внешней флеши он не нужен (см. ту же переменную ниже).
    variable::set("shared_flash", if (wants_ota && !wants_ext_ota) || (wants_config && store == "eeprom") { "true" } else { "false" });
    // Модуль флеша в bsp нужен, пока есть общий замок или разделы OTA; без обоих
    // файла и его документации быть не должно (`pub mod flash;` закрыт тем же
    // условием в lib.rs).
    if !wants_ota && !(wants_config && store == "eeprom") {
        file::delete("crates-cross/bsp/src/flash.rs");
        file::delete("docs/modules/bsp-flash.md");
    }
    variable::set("signed", if wants_ota { "true" } else { "false" });
    // Страницу обмена знает только раскладка, а её тут пишет пользователь —
    // угадывать нельзя. Пустая строка означает «неизвестна»:
    // xtask по ней не отдаёт host-target тесту адреса разделов, и тест
    // обмена сам себя пропускает. Подставить сюда правдоподобное число было
    // бы хуже молчания: на чипе с другим сектором тест искал бы прежний
    // образ не по тому адресу и падал бы, обвиняя bootloader.
    variable::set("page_size", "");
    // `ACTIVE`/`DFU` без раскладки не оборачиваются в `Paged`: страница обмена
    // здесь — страница цельного `Flash` (как было до расширения страницы).
    variable::set("swap_paged", "false");
    // Страница раздела `BOOTLOADER_STATE` (`adapters::flash::Paged`; число
    // уезжает переменной среды в crates-cross/.cargo/config.toml, её читают
    // bsp::flash и boot) — тоже знание раскладки. Её здесь нет,
    // поэтому — страница цельного `Flash`, как было до `Paged`: верно на
    // чипах с равными секторами, а на F2/F4/F7 `STATE` в мелком секторе
    // приведёт к панике `Partition::new` при старте — об этом печатается
    // предупреждение ниже.
    variable::set("state_erase_size", "embassy_stm32::flash::MAX_ERASE_SIZE");
    if wants_ota && !wants_ext_ota {
        print("BOOTLOADER_STATE: страница раздела — сектор всего чипа (MAX_ERASE_SIZE); если STATE");
        print("  лежит в более мелком секторе (F2/F4/F7), впишите его размер в STATE_ERASE_SIZE");
        print("  в [env] crates-cross/.cargo/config.toml (STATE_ERASE_SIZE; общая у приложения и bootloader'а), иначе паника при старте");
    }
    // Страница обмена с внешними разделами — тоже знание раскладки. Сектор
    // внешней NOR (4 KiB) — нижняя граница: верна, если ACTIVE в memory.x
    // кратен max(странице чипа, 4 KiB); иначе `assert_partitions` паникует.
    variable::set("ext_page_size", "4_096");
    if wants_ota && wants_ext_ota {
        print("ota=external: страница обмена EXT_PAGE_SIZE в [env] crates-cross/.cargo/config.toml — 4 KiB;");
        print("  впишите max(сектор чипа, стирание драйвера) и сделайте ACTIVE кратным ей");
    }
    // Раздел приложения здесь задаёт человек, значит и помещается ли в него
    // граф — знает только он. Ответ проходит как есть; не влезет — скажет
    // линкер.
    variable::set("graph", if wants_graph { "true" } else { "false" });
} else {
    let m = MEMORY_LAYOUT[suffix];

    // Второй банк flash за разрывом (H743 с 1 MiB и т.п.): с внутренней OTA
    // DFU лежит в нём, и прошивка пишет его через регион банка.
    let dfu_bank = m.contains("dfu_bank_origin");

    let pl = plan(m, erase_zero, WANTS, LIMITS);
    let in_flash = pl.in_flash;
    let config = pl.config;
    let key = pl.key;
    let ext_key = pl.ext_key;
    let ota_key = pl.ota_key;
    let ota_fits = pl.ota_fits;
    let ext_fits = pl.ext_fits;
    let ota = pl.ota;
    let ext_ota = pl.ext_ota;
    let settings_key = pl.settings_key;
    let app_length_key = pl.app_length_key;
    let graph = pl.graph;
    // Подпись есть всегда, когда есть OTA, и только тогда: подписывается то,
    // что приезжает в раздел DFU, и отдельного вопроса нет. Если схема не
    // поместилась, нет ни OTA, ни подписи.
    let signed = ota;

    if wants_config && !config {
        if erase_zero {
            print("Раздел настроек для " + suffix + " не выделен: у этого чипа флеш стирается в ноль,");
            print("  а sequential-storage считает стёртым состоянием 0xFF — хранилище молча не заработало бы");
        } else {
            print("Раздел настроек для " + suffix + " не выделен: под него нужны две страницы стирания, а их не остаётся");
        }
    }
    // Подсказки про внешнюю флешь: внутри схема не помещается, а снаружи
    // помещается — или помещается, но `ACTIVE` с внешней флешью заметно (в
    // полтора раза и больше) крупнее.
    if wants_ota && !wants_ext_ota && ext_fits {
        if !ota_fits {
            print("OTA во внутреннем flash не помещается, а с DFU/STATE на внешней флеши — да:");
            print("  ACTIVE " + m[ext_key + "active_length"] + "; сгенерируйте с --define ota=external");
        } else if size_in_bytes(m[ext_key + "active_length"]) * 2 >= size_in_bytes(m[ota_key + "active_length"]) * 3 {
            print("Подсказка: с DFU/STATE на внешней флеши ACTIVE был бы " + m[ext_key + "active_length"]
                + " вместо " + m[ota_key + "active_length"] + " (--define ota=external)");
        }
    }
    if in_flash {
        print("Раздел настроек: " + m[settings_key + "length"] + " с адреса " + m[settings_key + "origin"]);
    } else if config && store == "eeprom" {
        print("Настройки — во внутренней EEPROM чипа: флеш под них не тратится");
    } else if config && store == "external" {
        print("Настройки — на внешней флеши: подставьте свой драйвер в crates-cross/bsp/src/config.rs");
        print("  (пока там заглушка adapters::flash::Unconnected — проект собирается, настройки не сохраняются)");
    }
    if wants_graph && !graph {
        print("Граф задач отключён для " + suffix + ": под приложение отведено " + m[app_length_key] + ", а supervisor туда не помещается");
        print("  вернуть можно вручную — код графа лежит в docs/modules/app-graph.md");
    }

    variable::set("write_size", m["write_size"]);
    // `page_size` — страница обмена embassy-boot (`PAGE_SIZE`): по ней xtask
    // и host-target тест ищут прежний образ (он уезжает в `DFU[i + 1]`). С
    // внутренней OTA это `swap_page_size` её набора цифр — она крупнее
    // физической страницы чипа там, где chip-data-gen расширил её ради
    // журнала `STATE` (`swap_paged`): прошивка тогда оборачивает ACTIVE и DFU в
    // `adapters::flash::Paged<_, SWAP_PAGE_SIZE>` (= page_size). Без OTA страницы обмена нет —
    // остаётся физическая, и `swap_paged` = "false".
    // С внешними разделами страница обмена зависит от стирания драйвера, а
    // DFU в адресном пространстве МК нет вовсе — host-target тест обмена
    // (он читает DFU пробником) здесь не к чему применить. Пусто — «размер
    // неизвестен», тест сам себя пропускает; `swap_paged` = "false" (у внешней
    // OTA своя `ext_page_size`).
    let swap_page = m["page_size"];
    let swap_paged = false;
    if ota && !ext_ota {
        swap_page = m[ota_key + "swap_page_size"];
        swap_paged = m[ota_key + "swap_paged"] == "true";
    }
    variable::set("page_size", if ext_ota { "" } else { swap_page });
    variable::set("swap_paged", if swap_paged { "true" } else { "false" });
    // Сектор, в котором лежит `BOOTLOADER_STATE`: на F2/F4/F7 он мельче
    // страницы обмена, и раздел над цельным `Flash` не строился бы вовсе (см.
    // `compute_ota_partitions` в chip-data-gen). Без OTA его нет, как и
    // файлов, которые его читают; с внешней — его страницу несёт драйвер.
    if ota && !ext_ota {
        variable::set("state_erase_size", m[ota_key + "bootloader_state_erase_size"]);
    }
    // Шаг внешних разделов и страница обмена, которую раскладка ждёт от
    // драйвера: его `ERASE_SIZE` обязан её делить (const-assert рядом с ним).
    if ext_ota {
        variable::set("ext_page_size", m[ota_key + "page_size"]);
    }
    variable::set("ota", if ota { "true" } else { "false" });
    variable::set("ota_store", if ext_ota { "external" } else { "internal" });
    // DFU в банке 2 за разрывом: прошивка пишет его через `Bank2Region`, а не через
    // цельный `Flash` (см. `dfu_bank_origin` в chip-data-gen).
    variable::set("dfu_bank", if ota && !ext_ota && dfu_bank { "true" } else { "false" });
    variable::set("config", if config { "true" } else { "false" });
    variable::set("config_store", if config { store } else { "none" });
    if in_flash {
        variable::set("config_erase_size", m[settings_key + "erase_size"]);
    }
    // Общий объект `Flash` в bsp: его делят OTA во внутреннем flash и настройки
    // во флеше или EEPROM (EEPROM пишется через тот же контроллер). Внешней
    // флеши он не нужен — с `ota=external` приложение во внутренний flash не
    // пишет вовсе (`ACTIVE` пишет bootloader), — и без OTA создавать его
    // незачем: был бы неиспользуемый код.
    variable::set("shared_flash", if (ota && !ext_ota) || (config && store != "external") { "true" } else { "false" });
    if !ota && !(config && store != "external") {
        file::delete("crates-cross/bsp/src/flash.rs");
        file::delete("docs/modules/bsp-flash.md");
    }
    variable::set("signed", if signed { "true" } else { "false" });
    variable::set("graph", if graph { "true" } else { "false" });
    if !config {
        file::delete("crates-cross/bsp/src/config.rs");
    }

    // База flash одна на оба режима: и `__bootloader_*`, и `__config_*`
    // отсчитываются от неё, а не от ORIGIN(FLASH) конкретного крейта — так их
    // читают embassy-boot и embassy-stm32::flash.
    let flash_base = m["flash_origin"];
    let config_region = [];
    let config_section = [];
    if in_flash {
        config_region.push("    CONFIG            (rx)  : ORIGIN = " + m[settings_key + "origin"] + ", LENGTH = " + m[settings_key + "length"]);
        config_section = [
            "",
            "/* Раздел настроек (crates-cross/bsp/src/config.rs, sequential-storage). */",
            "/* Смещения от базы flash: embassy-stm32 адресует flash от неё. */",
            "__config_start = ORIGIN(CONFIG) - " + flash_base + ";",
            "__config_end   = ORIGIN(CONFIG) + LENGTH(CONFIG) - " + flash_base + ";",
        ];
    }

    // RAM одинаков в обоих крейтах и в обоих режимах. Следом — все остальные
    // регионы чипа (ITCM, AXISRAM, CCMRAM, BKPSRAM, EEPROM, OTP, окна внешних
    // шин...) готовыми строками из chip-data-gen: часть из них закомментирована,
    // причина указана прямо в строке.
    let ram = ["    RAM               (xrw) : ORIGIN = " + m["ram_origin"] + ", LENGTH = " + m["ram_length"]];
    if m.contains("panic_origin") {
        ram.push("    PANIC             (xrw) : ORIGIN = " + m["panic_origin"] + ", LENGTH = " + m["panic_length"]);
    }

    // Регион отдаётся только символами, без единой SECTIONS-директивы, и это
    // не стилистика. flip-link (линкер-обёртка, уводящая стек от
    // переполнения в статические данные) считает занятую часть RAM по адресам
    // секций: собственная секция с VMA в конце региона убеждает его, что
    // свободного места нет, и он оставляет _stack_start в самом начале RAM.
    // Прошивка после этого не доживает до первой строки лога — первый же push
    // уходит за нижнюю границу RAM, HardFault, LOCKUP. Проверено на
    // STM32F3Discovery: с секцией _stack_start = 0x20000000, без неё — 0x20009768.
    // Абсолютные символы ниже flip-link не смущают.
    let panic_section = [];
    if m.contains("panic_origin") {
        panic_section = [
            "",
            "/* Сюда пишет panic-persist (паникёр release-профиля) — по голым */",
            "/* адресам, без секции: секция здесь ломает flip-link, см. docs/diagnostics.md. */",
            "_panic_dump_start = ORIGIN(PANIC);",
            "_panic_dump_end   = ORIGIN(PANIC) + LENGTH(PANIC);",
        ];
    }
    if m.contains("extra_regions") {
        ram += m["extra_regions"];
    }

    if ext_ota {
        // Внутри — только ACTIVE (и, если так решено, раздел настроек);
        // `STATE` и `DFU` — на внешней флеши. Их символы — смещения от начала
        // ВНЕШНЕЙ флеши: embassy-boot читает их как смещения в том `NorFlash`,
        // над которым строит раздел, а там стоит драйвер микросхемы. Регионов
        // в `MEMORY` у них нет: в адресном пространстве МК внешней флеши нет.
        let base = flash_base;
        let ext_symbols = [
            "",
            "/* STATE и DFU — на внешней флеши (драйвер в bsp и boot). Смещения от её */",
            "/* начала; шаг " + m[ota_key + "align"] + " байт, страница обмена — " + m[ota_key + "page_size"] + ": */",
            "/* стирание драйвера обязано её делить. */",
            "__bootloader_state_start = " + m[ota_key + "state_offset"] + ";",
            "__bootloader_state_end   = " + m[ota_key + "state_offset"] + " + " + m[ota_key + "state_length"] + ";",
            "__bootloader_dfu_start   = " + m[ota_key + "dfu_offset"] + ";",
            "__bootloader_dfu_end     = " + m[ota_key + "dfu_offset"] + " + " + m[ota_key + "dfu_length"] + ";",
        ];
        if config && store == "external" {
            ext_symbols += [
                "",
                "/* Раздел настроек — на той же внешней флеши, за DFU. */",
                "__config_start = " + m[ota_key + "config_offset"] + ";",
                "__config_end   = " + m[ota_key + "config_offset"] + " + " + m[ota_key + "config_length"] + ";",
            ];
        }
        let app = [
            "/* Сгенерировано chip-select.rhai: приложение линкуется в ACTIVE, */",
            "/* с базы flash стартует bootloader (crates-cross/boot). */",
            "",
            "MEMORY {",
            "    FLASH             (rx)  : ORIGIN = " + m[ota_key + "active_origin"] + ", LENGTH = " + m[ota_key + "active_length"],
        ];
        app += config_region;
        app += ram;
        app.push("}");
        app.push("");
        app.push("/* ACTIVE — это и есть FLASH данного крейта. */");
        app.push("__bootloader_active_start = ORIGIN(FLASH) - " + base + ";");
        app.push("__bootloader_active_end   = ORIGIN(FLASH) + LENGTH(FLASH) - " + base + ";");
        app += ext_symbols;
        app += config_section;
        app += panic_section;
        file::write("crates-cross/app/memory.x", app);
        file::write("crates-cross/target-tests/memory.x", app);

        let boot = [
            "/* Сгенерировано chip-select.rhai: зона bootloader, до ACTIVE. */",
            "",
            "MEMORY {",
            "    FLASH             (rx)  : ORIGIN = " + m["flash_origin"] + ", LENGTH = " + m[ota_key + "bootloader_length"],
            "    ACTIVE            (rx)  : ORIGIN = " + m[ota_key + "active_origin"] + ", LENGTH = " + m[ota_key + "active_length"],
        ];
        boot += ram;
        boot.push("}");
        boot.push("");
        boot.push("__bootloader_active_start = ORIGIN(ACTIVE) - " + base + ";");
        boot.push("__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - " + base + ";");
        boot += ext_symbols;
        boot += panic_section;
        file::write("crates-cross/boot/memory.x", boot);
    } else if ota {
        // Смещения __bootloader_* отсчитываются от БАЗЫ FLASH, а не от
        // ORIGIN(FLASH) конкретного крейта: их читает embassy-boot, работающий
        // с флешем целиком. У app ORIGIN(FLASH) — это ACTIVE, у boot — база,
        // поэтому в обоих файлах стоит литерал базы.
        let base = flash_base;
        // DFU в банке 2 за разрывом пишется через регион банка (`Bank2Region`), у которого
        // нулевое смещение — начало банка, а не база flash: цельный `Flash` туда не
        // дотягивается (его `FLASH_SIZE` — сумма банков, меньше смещения банка 2).
        let dfu_base = if dfu_bank { m["dfu_bank_origin"] } else { base };

        // app линкуется В ACTIVE. Раньше оба крейта получали одинаковый
        // memory.x с FLASH от базы — и `cargo xtask flash` (сначала boot,
        // потом app) затирал bootloader образом приложения.
        let app = [
            "/* Сгенерировано chip-select.rhai: приложение линкуется в ACTIVE, */",
            "/* с базы flash стартует bootloader (crates-cross/boot). */",
            "",
            "MEMORY {",
            "    FLASH             (rx)  : ORIGIN = " + m[ota_key + "active_origin"] + ", LENGTH = " + m[ota_key + "active_length"],
            "    BOOTLOADER_STATE  (rx)  : ORIGIN = " + m[ota_key + "bootloader_state_origin"] + ", LENGTH = " + m[ota_key + "bootloader_state_length"],
            "    DFU               (rx)  : ORIGIN = " + m[ota_key + "dfu_origin"] + ", LENGTH = " + m[ota_key + "dfu_length"],
        ];
        app += config_region;
        app += ram;
        app.push("}");
        app.push("");
        app.push("__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - " + base + ";");
        app.push("__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - " + base + ";");
        app.push("");
        app.push("/* ACTIVE — это и есть FLASH данного крейта. */");
        app.push("__bootloader_active_start = ORIGIN(FLASH) - " + base + ";");
        app.push("__bootloader_active_end   = ORIGIN(FLASH) + LENGTH(FLASH) - " + base + ";");
        app.push("");
        app.push("__bootloader_dfu_start = ORIGIN(DFU) - " + dfu_base + ";");
        app.push("__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - " + dfu_base + ";");
        app += config_section;
        app += panic_section;
        file::write("crates-cross/app/memory.x", app);
        // Тесты на устройстве (crates-cross/target-tests) линкуются туда же, куда
        // приложение: probe-rs заливает тестовый образ на место app, и после
        // сброса в него прыгает тот же bootloader. Своей раскладки у них нет —
        // с базы flash тестовый образ затирал бы bootloader.
        file::write("crates-cross/target-tests/memory.x", app);

        // FLASH у boot — только его собственная зона, а не весь чип: тогда
        // разросшийся bootloader ловит линкер, а не молчаливое наложение на
        // ACTIVE.
        // Раздел настроек bootloader'у не нужен и в его memory.x не попадает:
        // он в него не пишет, а FLASH у него — только собственная зона.
        let boot = [
            "/* Сгенерировано chip-select.rhai: зона bootloader, до BOOTLOADER_STATE. */",
            "",
            "MEMORY {",
            "    FLASH             (rx)  : ORIGIN = " + m["flash_origin"] + ", LENGTH = " + m[ota_key + "bootloader_length"],
            "    BOOTLOADER_STATE  (rx)  : ORIGIN = " + m[ota_key + "bootloader_state_origin"] + ", LENGTH = " + m[ota_key + "bootloader_state_length"],
            "    ACTIVE            (rx)  : ORIGIN = " + m[ota_key + "active_origin"] + ", LENGTH = " + m[ota_key + "active_length"],
            "    DFU               (rx)  : ORIGIN = " + m[ota_key + "dfu_origin"] + ", LENGTH = " + m[ota_key + "dfu_length"],
        ];
        boot += ram;
        boot.push("}");
        boot.push("");
        boot.push("__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - " + base + ";");
        boot.push("__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - " + base + ";");
        boot.push("");
        boot.push("__bootloader_active_start = ORIGIN(ACTIVE) - " + base + ";");
        boot.push("__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - " + base + ";");
        boot.push("");
        boot.push("__bootloader_dfu_start = ORIGIN(DFU) - " + dfu_base + ";");
        boot.push("__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - " + dfu_base + ";");
        boot += panic_section;
        file::write("crates-cross/boot/memory.x", boot);
    } else {
        // OTA-схема embassy-boot в этот чип не помещается — почему именно,
        // сказано в "note" (посчитано chip-data-gen по реальным секторам).
        // Проект собирается одним образом на весь flash, crates-cross/boot в него не
        // попадает вовсе (см. [conditional] в cargo-generate.toml).
        // Причина у отказа две: либо схема не помещается (тогда её объясняет
        // "note" из MEMORY_LAYOUT), либо от неё отказались при генерации —
        // там объяснять нечего, сообщение уже напечатано выше.
        // Причина берётся по тому же ключу, что и вся раскладка: когда схему
        // вытеснил раздел настроек, объяснение лежит в "config_note", а
        // "note" у такого чипа нет вовсе — и в комментарий memory.x попадала
        // пустота (rhai на отсутствующем ключе молча отдаёт unit).
        let note_key = ota_key + "note";
        let reason = "отключён при генерации";
        if wants_ota {
            reason = if m.contains(note_key) { m[note_key] } else { "схема не помещается во flash" };
            // Отдельная причина: раскладка-то сходится, но в раздел под
            // приложение не влезает даже пустая прошивка шаблона. Общее
            // «схема не помещается» тут вводило бы в заблуждение — цифры
            // сошлись, не сошёлся смысл.
            if m.contains(ota_key + "active_length")
                && size_in_bytes(m[ota_key + "active_length"]) < OTA_MIN_ACTIVE
            {
                reason = "под приложение осталось бы " + m[ota_key + "active_length"]
                    + ", куда не помещается даже пустая прошивка";
            }
        }
        if wants_ota {
            print("OTA отключён для " + suffix + ": " + reason);
            print("  собирается один образ на весь flash, crates-cross/boot в проект не входит");
        }
        // Каталог убирается здесь, а не через [conditional] в
        // cargo-generate.toml: условия там вычисляются по значениям
        // [placeholders] и переменных, выставленных хуком, не видят
        // (проверено эмпирически — с `[conditional.'ota != "true"']` каталог
        // оставался на месте, а то же условие по `ci` работало). Удаление
        // идёт по временной копии шаблона, до рендера — как и file::write.
        file::delete("crates-cross/boot");
        // Вместе с bootloader'ом уходит и работа с разделом DFU: без OTA
        // раздела нет, а символы `__bootloader_dfu_*`, на которые опирается
        // embassy-boot, некому определить.
        // И релиз по тегу: выкладывать нечего, `app.bin` собирается только
        // там, где есть куда его доставлять (см. ту же причину выше).
        file::delete(".github/workflows/release.yml");

        // FLASH берётся по тому же ключу: с разделом настроек приложению
        // достаётся не весь чип, а flash минус две последние страницы.
        let single = [
            "/* Сгенерировано chip-select.rhai: один образ на весь flash. */",
            "/* OTA (crates-cross/boot) не подключён: " + reason + ". */",
            "",
            "MEMORY {",
            "    FLASH             (rx)  : ORIGIN = " + m["flash_origin"] + ", LENGTH = " + m[key + "flash_length"],
        ];
        single += config_region;
        single += ram;
        single.push("}");
        single += config_section;
        single += panic_section;
        // Boot удалён строкой выше, писать в него нечего; target-tests получают
        // ту же раскладку, что и app (см. ветку с OTA выше).
        file::write("crates-cross/app/memory.x", single);
        file::write("crates-cross/target-tests/memory.x", single);
    }
}

// Модуля графа в проекте без графа нет вовсе: всё его содержимое закрыто
// `{% if graph == "true" %}`, и остался бы файл с одной строкой
// `#![doc = include_str!(..)]` — модуль без кода, только ради документации.
// Сама документация никуда не девается (`docs/modules/app-graph.md`), по ней
// граф и возвращают вручную; см. сообщение выше, где он отключается по
// размеру раздела. Условие в `crates-cross/app/src/main.rs` (`mod graph;`)
// закрыто тем же `{% if %}`, поэтому удаление файла ничего не ломает.
if variable::get("graph") != "true" {
	file::delete("crates-cross/app/src/graph.rs");
}

// Протокол связи и интерфейс под ним. Проверяются здесь, в конце, а не при
// ответе: вопросы задаются до каскада, а допустимость зависит и от чипа (есть
// ли у него блок), и от графа (узлу протокола без него некуда встать — граф
// мог снять и размер раздела выше).
//
// Какие интерфейсы годятся протоколу и какой блок чипа каждый требует —
// таблицами; новый интерфейс добавляется строкой в обе.
const PROTOCOL_INTERFACES = #{
    "none": ["none"],
    "modbus": ["uart", "tcp", "udp", "usb"],
    "protobuf": ["uart", "tcp", "udp", "usb"],
    "canopen": ["can"],
};
// Значения через запятую — для сообщений об ошибке. `reduce` без начального
// значения начинает с `()`, и список получил бы ведущую запятую.
fn listed(values) {
    let out = "";
    for v in values {
        if out != "" { out += ", "; }
        out += v;
    }
    out
}
let chip_name = variable::get("chip");
let protocol = if variable::is_set("protocol") { variable::get("protocol") } else { "none" };
let iface = if variable::is_set("interface") { variable::get("interface") } else { "none" };
if !PROTOCOL_INTERFACES.contains(protocol) {
    throw "protocol: ожидается " + listed(PROTOCOL_INTERFACES.keys()) + ", а не «" + protocol + "»";
}
let fits = PROTOCOL_INTERFACES[protocol];
if !fits.contains(iface) {
    if protocol == "none" {
        throw "interface: интерфейс «" + iface + "» выбран без протокола — его не обслуживал бы ни один узел; выберите протокол или interface=none";
    }
    throw "interface: протокол " + protocol + " работает поверх " + listed(fits) + ", а не «" + iface + "»";
}
if protocol != "none" {
    if variable::get("graph") != "true" {
        throw "protocol: узлу " + protocol + " нужен граф задач, а он отключён (ответом graph=no или потому, что не помещается во flash " + chip_name + ")";
    }
    let blocks = INTERFACE_BLOCKS[iface];
    let found = [];
    for tag in INTERFACES[suffix] {
        if blocks.contains(tag) { found.push(tag); }
    }
    if found.is_empty() {
        throw "interface: у " + chip_name + " нет выведенного блока " + listed(blocks).to_upper() + " — интерфейс " + iface + " недоступен";
    }
    if !INSTANCES.contains(suffix) || !INSTANCES[suffix].contains(found[0]) {
        throw "interface: в chip-data/instances.rhai нет имени блока " + found[0] + " у " + chip_name + " — перегенерируйте таблицы (chip-data-gen)";
    }
    variable::set("iface_periph", INSTANCES[suffix][found[0]]);
    if iface == "can" {
        variable::set("can_kind", found[0]);
    }
    if iface == "usb" {
        variable::set("usb_kind", found[0]);
    }
}
variable::set("protocol", protocol);
variable::set("interface", iface);
// Master Modbus. Вопрос условный (`[conditional]` в cargo-generate.toml) и
// задаётся только при protocol=modbus; с другим протоколом переменной нет.
// Но `--define modbus_master=yes` при другом протоколе возможен — такое
// сочетание приводится к «нет» молча (как `signed` без OTA).
let wants_master = false;
if variable::is_set("modbus_master") {
    let answer = variable::get("modbus_master");
    wants_master = answer == "yes" || answer == "true";
}
let master = protocol == "modbus" && wants_master;
variable::set("modbus_master", if master { "true" } else { "false" });
// Файл master'а — только там, где он выбран; `pub mod master;`, порт и
// клиентские impl'ы вырезает Liquid в комментариях. Без modbus каталог
// `domain/src/modbus` уже удалён ниже целиком, второй раз его не трогаем.
if protocol == "modbus" && !master {
	file::delete("crates-host/domain/src/modbus/master.rs");
}
// Модуль заглушки UART в bsp — только под интерфейс uart: без него
// `bsp::serial` не объявлен, и файл остался бы мусором с Liquid внутри.
if iface != "uart" {
	file::delete("crates-cross/bsp/src/serial.rs");
	file::delete("docs/modules/bsp-serial.md");
}
// То же для CAN: модуль шины и его документация — только под интерфейс can.
if iface != "can" {
	file::delete("crates-cross/bsp/src/can.rs");
	file::delete("docs/modules/bsp-can.md");
}
// Крейты протоколов — только в проекте, где протокол выбран: без них нет ни
// схемы protobuf (и её генерации в `build.rs`), ни словаря CANopen. Ссылки на
// них в корневом `Cargo.toml`, `domain/Cargo.toml` и исходниках `domain`
// вырезает Liquid в комментариях вокруг них.
// Стек протокола целиком — порт, адаптер, узел, крейт примера — только в
// проекте, где он выбран: «modbus поверх uart» не тянет ни protobuf, ни CAN,
// а проект без протокола — ничего из них. Ссылки на удалённое (`mod`,
// `pub use`, зависимости, фичи) вырезает Liquid в комментариях.
if protocol != "modbus" {
	file::delete("crates-host/ports/src/modbus.rs");
	file::delete("crates-host/ports/src/modbus");
	file::delete("crates-host/adapters/src/modbus.rs");
	file::delete("crates-host/adapters/src/modbus");
	file::delete("crates-host/domain/src/modbus.rs");
	file::delete("crates-host/domain/src/modbus");
}
if protocol != "protobuf" {
	file::delete("crates-host/ports/src/host.rs");
	file::delete("crates-host/adapters/src/cobs.rs");
	file::delete("crates-host/domain/src/host.rs");
	file::delete("crates-host/domain/src/host");
	file::delete("crates-host/proto");
}
if protocol != "canopen" {
	file::delete("crates-host/ports/src/can.rs");
	file::delete("crates-host/domain/src/canopen.rs");
	file::delete("crates-host/domain/src/canopen");
	file::delete("crates-host/canopen");
}
if protocol == "none" {
	file::delete("crates-host/ports/src/link.rs");
}
if iface != "uart" {
	file::delete("crates-host/adapters/src/serial.rs");
}
if iface != "can" {
	file::delete("crates-host/adapters/src/can.rs");
}
// USB — устройство `embassy-usb`, узел USB и поток CDC — только под usb.
if iface != "usb" {
	file::delete("crates-cross/bsp/src/usb.rs");
	file::delete("docs/modules/bsp-usb.md");
	file::delete("crates-host/adapters/src/usb.rs");
	file::delete("crates-host/domain/src/usb.rs");
}
// Сеть — стек `embassy-net`, узел NET и их сокеты — только под tcp/udp.
if iface != "tcp" && iface != "udp" {
	file::delete("crates-cross/bsp/src/net.rs");
	file::delete("docs/modules/bsp-net.md");
	file::delete("crates-host/adapters/src/net.rs");
	file::delete("crates-host/adapters/src/net");
	file::delete("crates-host/domain/src/net.rs");
}
// RNG — только под tcp/udp и только если он есть у чипа (`rng_irq` не пуст).
if rng[0] == "" || (iface != "tcp" && iface != "udp") {
	file::delete("crates-cross/bsp/src/rng.rs");
	file::delete("docs/modules/bsp-rng.md");
}

// Слот драйвера внешней флеши в bsp — только у OTA с разделами на ней. Без
// него `bsp::external` не объявлен, и файл остался бы мусором с Liquid внутри.
if variable::get("ota") != "true" || variable::get("ota_store") != "external" {
	file::delete("crates-cross/bsp/src/external.rs");
}

// Подмодули bootloader'а под ответы — по той же причине: `mod` в
// `crates-cross/boot/src/main.rs` закрыт тем же `{% if %}`, а файл с Liquid
// остался бы мусором. Без OTA каталога `boot` нет вовсе.
if variable::get("ota") == "true" {
	if variable::get("ota_store") != "external" {
		file::delete("crates-cross/boot/src/external.rs");
	}
	if variable::get("signed") != "true" {
		file::delete("crates-cross/boot/src/signature.rs");
	}
}
