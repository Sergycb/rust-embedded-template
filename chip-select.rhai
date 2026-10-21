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

/// Число из таблицы раскладки с разделителями, как в Rust: `"131_072"`; без них
/// (`"131072"`) тоже годится. `parse_int` подчёркиваний не знает.
fn plain_number(text) {
    let raw = text;
    raw.replace("_", "");
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
    // под разделом (`flash_page::ErasePage` в bsp::settings).
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

/// Статическая RAM проекта без транспортов по плану `p` (см. `RAM_COST`):
/// ядро, OTA, хранилище настроек и граф.
fn ram_base(costs, p) {
    let total = costs.core;
    if p.ota { total += if p.ext_ota { costs.ota.external } else { costs.ota.internal }; }
    if p.config { total += costs.config[p.store]; }
    if p.graph { total += costs.graph; }
    total
}

/// Оценка статической RAM: `base` (`ram_base`), транспорты `links`
/// (`#{ kind, protocol }`) и сетевые службы `services`. Сетевой стек — один на
/// проект, сколько бы ни было `tcp`/`udp`.
fn ram_estimate(costs, base, links, services) {
    let total = base;
    let net = false;
    for link in links {
        total += costs.links[link.kind + ":" + link.protocol];
        if link.kind == "tcp" || link.kind == "udp" { net = true; }
    }
    if net { total += costs.net_stack; }
    for name in services { total += costs.services[name]; }
    total
}

/// Самый дорогой по RAM протокол поверх вида `kind`: список транспортов
/// показывается до вопроса о протоколе, и пункт оценивается по худшему.
fn worst_protocol(costs, kind, protocol_interfaces) {
    let worst = "none";
    for name in protocol_interfaces.keys() {
        if protocol_interfaces[name].contains(kind)
            && costs.links[kind + ":" + name] > costs.links[kind + ":" + worst]
        {
            worst = name;
        }
    }
    worst
}

/// Байты — в KiB с одним знаком после точки, для сообщений.
fn kib(bytes) {
    let tenths = (bytes * 10 + 512) / 1024;
    (tenths / 10).to_string() + "." + (tenths % 10).to_string() + " KiB"
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

/// Меню из нескольких вариантов: `texts` показываются как есть, а ответ —
/// значение с тем же индексом: `values[i]` соответствует `texts[i]`. Без
/// терминала (`--silent`, CI) `variable::prompt` отвечает ошибкой «not a
/// terminal», и тогда берётся `default_value`: вопрос, который некому задать, не
/// должен ронять генерацию. Любая другая ошибка (прерванный выбор) пробрасывается.
fn ask(label, values, texts, default_value) {
    let default_text = texts[0];
    for i in 0..values.len() {
        if values[i] == default_value { default_text = texts[i]; }
    }
    let pick = default_text;
    try {
        pick = variable::prompt(label, default_text, texts);
    } catch (err) {
        if !err.to_string().contains("not a terminal") {
            throw err;
        }
    }
    for i in 0..texts.len() {
        if texts[i] == pick { return values[i]; }
    }
    default_value
}

/// Список с отметками: отмеченные `values` в порядке пунктов. В хуках
/// `cargo-generate` есть только одиночный выбор, поэтому отметка `[x] `/`[ ] `
/// нарисована в тексте пункта (`value — text`), а меню после каждого выбора
/// показывается заново; `готово` (оно же умолчание и ответ без терминала)
/// закрывает список.
fn checklist(label, values, texts) {
    checklist(label, values, texts, |value, chosen| "")
}

/// То же с припиской к пункту, которая зависит от уже отмеченных:
/// `note.call(value, chosen)` — строка после текста пункта `value` при
/// отмеченных `chosen`; считается заново при каждой перерисовке.
fn checklist(label, values, texts, note) {
    let marked = values.map(|value| false);
    loop {
        let chosen = [];
        for i in 0..values.len() {
            if marked[i] { chosen.push(values[i]); }
        }
        let shown_values = ["готово"];
        let shown_texts = ["готово"];
        for i in 0..values.len() {
            shown_values.push(values[i]);
            shown_texts.push((if marked[i] { "[x] " } else { "[ ] " }) + values[i] + " — " + texts[i] + note.call(values[i], chosen));
        }
        let pick = ask(label, shown_values, shown_texts, "готово");
        if pick == "готово" { break; }
        for i in 0..values.len() {
            if values[i] == pick { marked[i] = !marked[i]; }
        }
    }
    let out = [];
    for i in 0..values.len() {
        if marked[i] { out.push(values[i]); }
    }
    out
}

/// Строка открытого ключа ed25519 из `~/.ssh/id_ed25519.pub`: `ssh-ed25519`,
/// блоб — 51 байт, то есть ровно 68 символов base64 без `=`, — и
/// необязательный комментарий. Кавычек, `\` и переводов строк нет: строка
/// уезжает в исходники шаблона.
fn ssh_key_ok(key, blob_prefix, base64_chars) {
    if key.contains("\"") || key.contains("\\") || key.contains("\n") || key.contains("\r") { return false; }
    let parts = key.split(" ");
    if parts.len() < 2 || parts[0] != "ssh-ed25519" { return false; }
    let blob = parts[1];
    if blob.len() != 68 || !blob.starts_with(blob_prefix) { return false; }
    for c in blob.chars() {
        if !base64_chars.contains(c) { return false; }
    }
    true
}

/// Свободные блоки вида `kind`: `#{ tag, block }` по меткам `tags` вида, какие
/// есть у чипа в `have` (в порядке `have`, внутри метки — в порядке `instances`),
/// без блоков, уже отданных транспортам `links` этого вида. `tcp` и `udp` делят
/// один ETH, поэтому блок занят только транспортом того же вида: каждый из них
/// свободен, пока сам не выбран.
fn free_blocks(kind, tags, links, have, instances) {
    let out = [];
    for tag in have {
        if !tags.contains(tag) { continue; }
        if !instances.contains(tag) {
            throw "interface: в chip-data/instances.rhai нет имён блоков " + tag + " — перегенерируйте таблицы (chip-data-gen)";
        }
        for block in instances[tag] {
            let busy = false;
            for link in links {
                if link.kind == kind && link.block == block { busy = true; }
            }
            if !busy { out.push(#{ tag: tag, block: block }); }
        }
    }
    out
}

/// Разбор `--define links=вид[:протокол],…`: `#{ kind, protocol }` в порядке
/// ответа, без протокола — `none`. Пробелы вокруг частей допускаются, пустой
/// элемент — нет. Хватает ли чипу блоков, здесь не проверяется: чип ещё не
/// выбран, это проверяет назначение блоков (и отсев чипов до каскада).
fn parse_links(text, kinds, protocol_interfaces) {
    let out = [];
    let chosen = "none";
    for item in text.split(",") {
        let kind = item;
        let protocol = "none";
        let colon = item.index_of(":");
        if colon >= 0 {
            kind = item.sub_string(0, colon);
            protocol = item.sub_string(colon + 1);
            protocol.trim();
        }
        kind.trim();
        if kind == "" && colon < 0 {
            throw "links: пустой элемент в «" + text + "»";
        }
        if !kinds.contains(kind) {
            throw "links: ожидается вид " + listed(kinds) + ", а не «" + kind + "»";
        }
        if !protocol_interfaces.contains(protocol) {
            throw "links: ожидается протокол " + listed(protocol_interfaces.keys()) + ", а не «" + protocol + "»";
        }
        if !protocol_interfaces[protocol].contains(kind) {
            throw "links: протокол " + protocol + " работает поверх " + listed(protocol_interfaces[protocol]) + ", а не «" + kind + "»";
        }
        if protocol != "none" {
            // Статики стека `zencan` — по одному на прошивку.
            if protocol == "canopen" && chosen == "canopen" {
                throw "links: CANopen пока только на одной шине CAN";
            }
            if chosen != "none" && chosen != protocol {
                throw "links: протокол в проекте пока один, а заданы " + chosen + " и " + protocol;
            }
            chosen = protocol;
        }
        out.push(#{ kind: kind, protocol: protocol });
    }
    out
}

/// Сколько транспортов каждого вида запрошено: `#{ вид: число }`.
fn link_counts(links) {
    let counts = #{};
    for link in links {
        counts[link.kind] = if counts.contains(link.kind) { counts[link.kind] + 1 } else { 1 };
    }
    counts
}

/// Значение переменной `links`: `вид:метка:блок:протокол:имя` через запятую.
/// Имя — транспорта в проекте: блок строчными (`usart2`, `usb_otg_hs`), а у
/// `tcp`/`udp`, которые делят один ETH, — сам вид.
fn links_text(links) {
    let out = "";
    for link in links {
        if out != "" { out += ","; }
        let name = if link.kind == "tcp" || link.kind == "udp" { link.kind } else { link.block.to_lower() };
        out += link.kind + ":" + link.tag + ":" + link.block + ":" + link.protocol + ":" + name;
    }
    out
}

/// Ответ `modbus_master`: спрашивается один раз, когда в проекте впервые
/// выбран Modbus; `--define` проверяется строго.
fn modbus_master_answer() {
    if variable::is_set("modbus_master") {
        let answer = variable::get("modbus_master");
        if answer != "yes" && answer != "true" && answer != "no" && answer != "false" {
            throw "modbus_master: ожидается yes или no, а не «" + answer + "»";
        }
        return answer == "yes" || answer == "true";
    }
    ask(
        "modbus_master: Modbus master (клиент) в дополнение к slave",
        ["no", "yes"],
        ["no — только slave", "yes — ещё и master (клиентские функции и транспорты)"],
        "no"
    ) == "yes"
}

/// Значения через запятую — для сообщений об ошибке. `reduce` без начального
/// значения начинает с `()`, и список получил бы ведущую запятую.
fn listed(values) {
    let out = "";
    for v in values {
        if out != "" { out += ", "; }
        out += v;
    }
    out
}

/// Адрес из таблицы раскладки: `"0x08008000"`. `parse_int` в rhai понимает
/// только десятичные числа без префикса, поэтому шестнадцатеричный разбор свой.
fn hex_value(text) {
    parse_int(text.sub_string(2), 16)
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

// Источник rust-lib (supervisor, fsm, watchdog, sync-request) — одна переменная
// на все места: оба Cargo.toml, allow-git в deny.toml, ссылки в документации.
// Форк подставляется `--define rust_lib=https://…/rust-lib`; меню нет — вопрос
// до выбора чипа никому не нужен. Хранится без `.git` и завершающего `/`:
// Cargo.toml дописывает `.git` сам, а allow-git сравнивает адрес без него.
let rust_lib = if variable::is_set("rust_lib") { variable::get("rust_lib") } else { "https://github.com/Sergycb/rust-lib" };
while rust_lib.ends_with("/") {
    rust_lib.pop();
}
if rust_lib.ends_with(".git") {
    rust_lib = rust_lib.sub_string(0, rust_lib.len() - 4);
}
if rust_lib == "" {
    throw "rust_lib: пустой адрес репозитория rust-lib";
}
variable::set("rust_lib", rust_lib);
// Перевод строки для корневого Cargo.toml: Liquid в корневом workspace живёт в
// `#`-комментариях, и рабочую строку с `{{rust_lib}}` там иначе не получить —
// `#{{newline}}строка` в шаблоне комментарий, а после рендера — `#` и строка
// (MAINTAINING.md, «Значение в корневом workspace»).
variable::set("newline", "\n");

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

// Кэши, которые bsp включает сам (`bsp::cache::enable()`, docs/clocks.md):
// теги через пробел — `icen`/`dcen` (кэши акселератора флеша, биты FLASH_ACR),
// `icache` (блок ICACHE). Из метаданных stm32-metapac, не из имени чипа. Кэши
// ядра Cortex-M7 (`scb`) добавляются ниже по `cpu`: ядро уже выбрано каскадом.
import "chip-data/cache" as cache_data;
const CACHE = cache_data::CACHE;

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
// Где лежат настройки (`config_store`, на него смотрит bsp::settings):
//   "flash"    — раздел во внутреннем флеше (как описано выше);
//   "eeprom"   — внутренняя EEPROM, если она у чипа есть (L0/L1): флеш не
//                тратится вовсе, и это единственное место под настройки на
//                чипах, чей флеш стирается в ноль;
//   "external" — внешняя флеш: ответ `external`; шаблон кладёт заглушку
//                (`bsp::stub::Flash`), драйвер подставляете вы;
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
    if answer != "yes" && answer != "true" && answer != "no" && answer != "false" {
        throw "graph: ожидается yes или no, а не «" + answer + "»";
    }
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

// Какие интерфейсы годятся протоколу. Без протокола («none») годится любой: проект
// получает заглушку транспорта и его фоновый узел (`NET`, `USB`), а протокол —
// ваш. Новый интерфейс добавляется строкой здесь и в `INTERFACE_BLOCKS`.
const PROTOCOL_INTERFACES = #{
    "none": ["uart", "can", "tcp", "udp", "usb"],
    "modbus": ["uart", "tcp", "udp", "usb"],
    "protobuf": ["uart", "tcp", "udp", "usb"],
    "canopen": ["can"],
};
// Виды транспорта по порядку.
const LINK_KINDS = ["uart", "can", "tcp", "udp", "usb"];
// Пункты списка транспортов по порядку: значение, вид транспорта и описание
// (после `[ ] значение — `). Вид — по одному пункту: второй транспорт того же
// вида и `udp` задаёт `--define links=...`. `ethernet` пока даёт `tcp`.
const TRANSPORT_ITEMS = [
    #{ value: "uart", kind: "uart", text: "UART (RS-485 — тот же UART с выводом DE)" },
    #{ value: "can", kind: "can", text: "шина CAN" },
    #{ value: "ethernet", kind: "tcp", text: "Ethernet (сеть)" },
    #{ value: "usb", kind: "usb", text: "USB CDC ACM (виртуальный COM-порт)" },
];
// Сетевые службы поверх стека (`net_services`): не транспорты с протоколом, а
// узлы на своих сокетах общего стека — спрашиваются, только когда отмечен
// Ethernet. SSH — криптография, поэтому только у чипа с RNG.
const NET_SERVICE_ITEMS = [
    #{ value: "ssh", text: "консоль SSH (sunset)", needs_rng: true },
    #{ value: "rest", text: "REST API по HTTP (picoserve)", needs_rng: false },
];
// Начало base64-блоба открытого ключа ed25519 (`ssh_key`): длина и имя типа,
// затем длина ключа (32). Других ключей `sunset` без фич не проверяет.
const SSH_KEY_BLOB_PREFIX = "AAAAC3NzaC1lZDI1NTE5AAAAI";
const BASE64_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PROTOCOL_TEXT = #{
    "modbus": "modbus — Modbus slave",
    "protobuf": "protobuf — сообщения protobuf от хоста",
    "canopen": "canopen — узел CANopen (стек zencan)",
};

// Статическая RAM (`.data` + `.bss` + `.uninit` release-образа `app`) по частям
// проекта, байт. Числа замерены, а не выведены: строка `ram:` из `cargo xtask
// build` по случаям `template-check`, стоимость части — разность пары сборок
// «с ней / без неё» на одном чипе, из нескольких чипов — наибольшая. Как
// перемерить — MAINTAINING.md, «Оценка RAM». Оценку проверяет `template-check`:
// у каждого случая она не ниже факта.
const RAM_COST = #{
    // Без OTA, настроек, графа и транспортов: stm32f407ve ota=no graph=no — 1344
    // (stm32l011f4 — 1316).
    core: 1344,
    // stm32f407ve: 3880 с OTA против 1952 без неё; внешняя — stm32h723ve 3832
    // (ota=external config=external) против 1988 (config=external).
    ota: #{ internal: 1928, external: 1844 },
    // По месту хранения (`config_store`): stm32f407ve 3912 против 3880,
    // stm32l073rz (EEPROM) 3868 против 3860, stm32h723ve 1988 против 1988.
    config: #{ flash: 32, eeprom: 8, external: 0 },
    // stm32g474re 3884 против 3084 без графа (stm32f407ve без OTA — 608).
    graph: 800,
    // Транспорт `вид:протокол` с его узлом, с графом (без графа — дешевле):
    // разность с проектом без транспортов на том же чипе. Для tcp/udp — за
    // вычетом `net_stack`.
    links: #{
        // stm32g474re uart-only 3884 против 3884; Modbus — stm32h743zg два UART и
        // TCP 13940 (по одному — 2168); protobuf — stm32f407ve 6824 против 3880.
        "uart:none": 0, "uart:modbus": 2200, "uart:protobuf": 2944,
        // stm32f407ve modbus-uart-can 6048 = 3880 + 2168 (UART с Modbus);
        // CANopen — stm32g474re canopen-config 7504 против 3884 + 32 (настройки).
        "can:none": 0, "can:canopen": 3588,
        // stm32h743zg против 3932: tcp-only 5308, modbus-tcp 9540 (с master и
        // без — одинаково); protobuf — stm32f407ve UART и TCP 13216.
        "tcp:none": 416, "tcp:modbus": 4648, "tcp:protobuf": 5432,
        // stm32h743zg: udp-only 5308, modbus-udp 9700; protobuf — stm32f407ve
        // protobuf-udp 9968 против 3880.
        "udp:none": 416, "udp:modbus": 4808, "udp:protobuf": 5128,
        // stm32g474re usb-only 5244 против 3884; Modbus — stm32f407ve 7464 против
        // 3880; protobuf — stm32f407ve два USB 12656 (по 4388).
        "usb:none": 1360, "usb:modbus": 3584, "usb:protobuf": 4388,
    },
    // Стек `embassy-net` и драйвер Ethernet — один раз на tcp/udp: stm32f407ve
    // modbus-tcp-udp 14296 — на 960 меньше, чем TCP и UDP по отдельности.
    net_stack: 960,
    // Сетевые службы (`NET_SERVICE_ITEMS`): узлов в шаблоне пока нет — 0;
    // появятся — замерить так же.
    services: #{ ssh: 0, rest: 0 },
};
// Сверх статической RAM — под стек: он в `.data`/`.bss` не входит (flip-link
// кладёт его под ними). Не замерен — без платы глубину стека не снять, — а
// выбран: futures задач `embassy` и буферы транспортов лежат в `.bss`, так что на
// стеке только цепочка `poll`, кадры прерываний и форматирование паники.
// Ошибка в меньшую сторону не тихая: под flip-link переполнение стека — HardFault,
// а не порча статиков. Больше — и Modbus по UART не взял бы STM32C011D6 (6 KiB).
const STACK_RESERVE = 1024;

// Ответы отсекают чипы ещё до каскада: сетевой стек просит чип с Ethernet,
// OTA — чип, где схема помещается, граф — где он влезает в раздел
// приложения. Каскад по символам тогда предлагает только то, с чем ответы
// выполнимы, а не отказывает в конце, когда выбор уже сделан. Чип, заданный
// напрямую (`--define chip_feature=...`), не фильтруется: там остаются
// прежние отказы с объяснением.
const WANTED_INTERFACE = if variable::is_set("interface") { variable::get("interface") } else { "none" };
// `--define links=...` — несколько транспортов одной строкой, вместо пары
// `interface` + `protocol`. Разбирается до каскада: по нему же отсеиваются чипы.
if variable::is_set("links") && (variable::is_set("interface") || variable::is_set("protocol")) {
    throw "links: задаётся вместо interface и protocol, а не вместе с ними";
}
const WANTED_LINKS = if variable::is_set("links") {
    parse_links(variable::get("links"), INTERFACE_BLOCKS.keys(), PROTOCOL_INTERFACES)
} else {
    []
};
const WANTED_COUNTS = link_counts(WANTED_LINKS);
let allowed = CHIPS;
if !variable::is_set("chip_feature")
    && (NEED.ota || NEED.config || NEED.graph || WANTED_INTERFACE != "none" || WANTED_LINKS.len() > 0)
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
        // Каждого вида из `links` у чипа должно быть не меньше блоков, чем запрошено.
        let instances = if INSTANCES.contains(chip) { INSTANCES[chip] } else { #{} };
        for kind in WANTED_COUNTS.keys() {
            if free_blocks(kind, INTERFACE_BLOCKS[kind], [], INTERFACES[chip], instances).len() < WANTED_COUNTS[kind] {
                return false;
            }
        }
        if WANTED_INTERFACE != "none" && INTERFACE_BLOCKS.contains(WANTED_INTERFACE) {
            let have = INTERFACES[chip];
            let any_block = false;
            for tag in INTERFACE_BLOCKS[WANTED_INTERFACE] {
                if have.contains(tag) { any_block = true; }
            }
            if !any_block { return false; }
        }
        let m = layouts[chip.sub_string(0, 2)][chip];
        // И транспорты из `links` должны поместиться в RAM вместе с тем, что
        // получится из остальных ответов.
        if WANTED_LINKS.len() > 0 {
            let base = ram_base(RAM_COST, plan(m, m["erase_zero"] == "true", WANTS, LIMITS));
            if ram_estimate(RAM_COST, base, WANTED_LINKS, []) + STACK_RESERVE > size_in_bytes(m["ram_length"]) {
                return false;
            }
        }
        fits_answers(m, m["erase_zero"] == "true", WANTS, NEED, LIMITS)
    });
    if allowed.len() == 0 {
        throw "ни один из " + CHIPS.len() + " чипов не подходит под ответы ("
            + (if WANTED_LINKS.len() > 0 { "links=" + variable::get("links") } else { "interface=" + WANTED_INTERFACE })
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

// Ядра Armv6-M/Armv8-M Baseline (Cortex-M0, M0+, M23): без счётчика циклов `DWT`,
// без регистров причины фолта (`CFSR`/`HFSR`) и без ловушки деления `CCR.DIV_0_TRP`.
// По этому признаку `bsp` собирает `cycles` и ветки `fault`, а `ports` — `CycleCounter`.
let cpu_core = variable::get("cpu");
variable::set("core_baseline", if cpu_core == "cortex-m0" || cpu_core == "cortex-m0plus" || cpu_core == "cortex-m23" { "true" } else { "false" });

// Кэши (см. CACHE выше). Без переопределения через --define: значение целиком
// выводится из chip_feature и cpu. Теги `scb` (кэш инструкций ядра M7) и `scb_d`
// (кэш данных ядра M7 вместе с регионом `DMA_NC`, см. ниже) добавляются тут и
// после раскладки; в плейсхолдер `cache` итог уходит там же, после неё.
let cache = if CACHE.contains(suffix) { CACHE[suffix] } else { "" };
if variable::get("cpu") == "cortex-m7" {
    // M7 с кэшем флеша не бывает, но склейка не должна на этом держаться.
    cache += if cache == "" { "scb" } else { " scb" };
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
// и DFU прошивка оборачивает в ErasePage ради малого журнала STATE.
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

// ============================================================================
// Вопросы про OTA, настройки, граф, транспорт и протокол — после выбора чипа.
//
// Чип уже известен, поэтому каждое меню предлагает только то, что для него
// возможно, а невозможного выбрать нельзя вовсе:
//   * граф, который не влезает в раздел приложения, не предлагается;
//   * интерфейс, блока которого у чипа нет, не предлагается;
//   * протоколы показываются только те, что работают поверх выбранного транспорта;
//   * OTA и настройки, которые на чип не помещаются, остаются в меню с
//     пометкой «НЕ ПОМЕЩАЕТСЯ»: выбрав их, пользователь получает ручную
//     раскладку памяти (как при `--define write_size=...`) и сам решает, что
//     менять.
// Ответ, пришедший через `--define`, — режим для скриптов, и проверяется строго:
// опечатка и невозможное для чипа значение остаются ошибкой с объяснением.
// Без терминала и без `--define` вопрос получает умолчание.
// ============================================================================
let layout = MEMORY_LAYOUT[suffix];
let chip_name = variable::get("chip");
let protocol = "none";
let iface = "none";
let wants_master = false;

// --- OTA
if !variable::is_set("ota") {
    // Настройки, заданные через `--define`, уже известны и меняют размер `ACTIVE`;
    // заданные в меню спрашиваются ниже, и тогда их цену покажет их собственный пункт.
    let plan_int = plan(layout, erase_zero, #{ ota: true, ext_ota: false, config: wants_config, external: wants_external, graph: false }, LIMITS);
    let plan_ext = plan(layout, erase_zero, #{ ota: true, ext_ota: true, config: wants_config, external: wants_external, graph: false }, LIMITS);
    let text_int = if plan_int.ota {
        "internal — DFU во flash чипа, ACTIVE " + layout[plan_int.ota_key + "active_length"]
    } else {
        "internal — НЕ ПОМЕЩАЕТСЯ на этом чипе: раскладку памяти придётся писать вручную"
    };
    let text_ext = if plan_ext.ext_ota {
        "external — DFU и STATE на внешней флеши, ACTIVE " + layout[plan_ext.ota_key + "active_length"]
    } else {
        "external — НЕ ПОМЕЩАЕТСЯ на этом чипе: раскладку памяти придётся писать вручную"
    };
    let answer = ask(
        "ota: OTA (bootloader и разделы ACTIVE/DFU)",
        ["internal", "external", "no"],
        [text_int, text_ext, "no — без OTA: один образ на весь flash"],
        if plan_int.ota { "internal" } else { "no" }
    );
    wants_ext_ota = answer == "external";
    wants_ota = answer != "no";
    if !wants_ota {
        print("OTA отключён по вашему выбору: один образ на весь flash, crates-cross/boot в проект не входит");
    }
}

// --- Настройки
if !variable::is_set("config") {
    let plan_cfg = plan(layout, erase_zero, #{ ota: wants_ota, ext_ota: wants_ext_ota, config: true, external: false, graph: false }, LIMITS);
    let kind = config_store(layout, true, false, EEPROM_MIN);
    let values = ["no"];
    let texts = ["no — без хранилища настроек"];
    // Флеш чипа, стирающийся в ноль, `sequential-storage` не годится (он считает
    // стёртым 0xFF), а EEPROM его обходит; внутреннего варианта без неё нет.
    if kind == "eeprom" || !erase_zero {
        values.push("internal");
        if kind == "eeprom" {
            texts.push("internal — внутренняя EEPROM чипа: flash под настройки не тратится");
        } else if plan_cfg.config {
            let note = if wants_ota && !plan_cfg.ota {
                " (но тогда OTA не поместится: раскладку памяти придётся писать вручную)"
            } else {
                ""
            };
            texts.push("internal — раздел " + layout[plan_cfg.settings_key + "length"] + " во flash" + note);
        } else {
            texts.push("internal — НЕ ПОМЕЩАЕТСЯ на этом чипе: раскладку памяти придётся писать вручную");
        }
    }
    values.push("external");
    texts.push("external — внешняя флеш: драйвер своей микросхемы подставляете вы");
    let answer = ask("config: хранилище настроек (sequential-storage)", values, texts, "no");
    wants_external = answer == "external";
    wants_config = answer != "no";
} else if wants_config && !wants_external && config_store(layout, true, false, EEPROM_MIN) == "flash" && erase_zero {
    throw "config: у " + chip_name + " флеш стирается в ноль, а sequential-storage считает стёртым 0xFF — хранилище молча не заработало бы; выберите config=external или config=no";
}

// --- Граф. Помещается ли он, зависит от выбранных OTA и настроек.
let wants_now = #{ ota: wants_ota, ext_ota: wants_ext_ota, config: wants_config, external: wants_external, graph: true };
let plan_now = plan(layout, erase_zero, wants_now, LIMITS);
// Выбор, который на чип не помещается: размер раздела приложения тогда знает
// только человек, поэтому граф не отсекается (как в ручной раскладке ниже).
let manual_now = variable::is_set("write_size") || (wants_ota && !plan_now.ota) || (wants_config && !plan_now.config);
if variable::is_set("graph") {
    if wants_graph && !manual_now && !plan_now.graph {
        throw "graph: у " + chip_name + " под приложение отведено " + layout[plan_now.app_length_key] + ", а граф задач (supervisor) туда не помещается; выберите graph=no";
    }
} else if manual_now || plan_now.graph {
    let answer = ask(
        "graph: граф задач supervisor (порядок старта, рестарты, watchdog)",
        ["yes", "no"],
        ["yes — граф задач и сторож", "no — без графа: main заканчивается после инициализации"],
        "yes"
    );
    wants_graph = answer == "yes";
    if !wants_graph {
        print("Граф задач отключён по вашему выбору: main заканчивается после инициализации");
    }
} else {
    wants_graph = false;
    print("Граф задач не предлагается: у " + chip_name + " под приложение отведено " + layout[plan_now.app_length_key]
        + ", а supervisor туда не помещается");
}

// --- Транспорты. Транспорт — `#{ kind, tag, block, protocol }`: вид, метка
// блока (chip-data/interfaces.rhai), блок чипа и протокол поверх него. Блок
// назначает генерация — первый свободный блок вида из списка чипа. Протокол в проекте
// пока один (`protocol`): первый выбранный, следующие транспорты — с ним или
// без протокола. Меню предлагают только виды, у которых есть блок.
let have = INTERFACES[suffix];
let instances = if INSTANCES.contains(suffix) { INSTANCES[suffix] } else { #{} };
// RAM: статическая без транспортов по итоговым OTA, настройкам и графу, и
// объём региона `RAM`. С ручной раскладкой регион пишет человек — оценки нет
// (`ram_limit` = `()`), и ничего не помечается.
let ram_base_now = ram_base(RAM_COST, plan(layout, erase_zero, #{ ota: wants_ota, ext_ota: wants_ext_ota, config: wants_config, external: wants_external, graph: wants_graph }, LIMITS));
let ram_limit = if manual_now { () } else { size_in_bytes(layout["ram_length"]) };
let links = [];
let wanted_protocol = if variable::is_set("protocol") { variable::get("protocol") } else { "" };
if wanted_protocol != "" && !PROTOCOL_INTERFACES.contains(wanted_protocol) {
    throw "protocol: ожидается " + listed(PROTOCOL_INTERFACES.keys()) + ", а не «" + wanted_protocol + "»";
}
if WANTED_LINKS.len() > 0 {
    // `--define links=...`: блоков каждого вида должно хватить на все запрошенные.
    for kind in WANTED_COUNTS.keys() {
        let all = free_blocks(kind, INTERFACE_BLOCKS[kind], [], have, instances);
        if all.len() < WANTED_COUNTS[kind] {
            let names = [];
            for b in all { names.push(b.block); }
            throw "links: у " + chip_name + " блоков вида " + kind + " — " + all.len()
                + (if names.len() > 0 { " (" + listed(names) + ")" } else { "" })
                + ", а запрошено " + WANTED_COUNTS[kind];
        }
    }
    for wanted in WANTED_LINKS {
        let free = free_blocks(wanted.kind, INTERFACE_BLOCKS[wanted.kind], links, have, instances);
        links.push(#{ kind: wanted.kind, tag: free[0].tag, block: free[0].block, protocol: wanted.protocol });
        if wanted.protocol != "none" { protocol = wanted.protocol; }
    }
    if protocol == "modbus" { wants_master = modbus_master_answer(); }
} else {
    // Транспорты по порядку: `#{ kind, name }` — вид и как его назвать в вопросе о
    // протоколе (пункт списка, например `ethernet`). Блоки им раздаются ниже, по
    // одному правилу с `--define links=...`.
    let wanted = [];
    if variable::is_set("interface") {
        // `interface` и `protocol` через `--define` — по-старому одна пара.
        let kind = variable::get("interface");
        if kind != "none" && !INTERFACE_BLOCKS.contains(kind) {
            throw "interface: ожидается none, " + listed(INTERFACE_BLOCKS.keys()) + ", а не «" + kind + "»";
        }
        if kind != "none" && free_blocks(kind, INTERFACE_BLOCKS[kind], [], have, instances).len() == 0 {
            throw "interface: у " + chip_name + " нет выведенного блока " + listed(INTERFACE_BLOCKS[kind]).to_upper() + " — интерфейс " + kind + " недоступен";
        }
        if kind != "none" { wanted.push(#{ kind: kind, name: kind }); }
    } else if wanted_protocol != "" && wanted_protocol != "none" {
        // Протокол задан `--define`, а транспорт нет: аппаратную часть молча не
        // выбирают — `--define` строгий, транспорт задаётся тоже.
        let usable = LINK_KINDS.filter(|name| PROTOCOL_INTERFACES[wanted_protocol].contains(name)
            && free_blocks(name, INTERFACE_BLOCKS[name], [], have, instances).len() > 0);
        if usable.len() == 0 {
            throw "protocol: у " + chip_name + " нет транспорта для протокола " + wanted_protocol + " (нужен " + listed(PROTOCOL_INTERFACES[wanted_protocol]) + ")";
        }
        throw "protocol: протокол " + wanted_protocol + " нужен транспорт — задайте interface (" + listed(usable) + ")";
    } else {
        // Список с отметками (`checklist()`). Пункт — вид, у которого есть блок;
        // имени блока в пункте нет: транспорт получает первый блок вида, а
        // сменить блок — дело `resources.rs`.
        let items = TRANSPORT_ITEMS.filter(|item| free_blocks(item.kind, INTERFACE_BLOCKS[item.kind], [], have, instances).len() > 0);
        // Пункт, с которым отмеченные (и он сам) не помещаются в RAM, помечен,
        // но выбрать его можно — тогда в конце печатается предупреждение.
        // Протокол ещё не спрошен, поэтому пункт стоит как его худший протокол.
        let ram_note = |value, chosen| {
            if ram_limit == () { return ""; }
            let set = [];
            for item in items {
                if item.value == value || chosen.contains(item.value) {
                    set.push(#{ kind: item.kind, protocol: worst_protocol(RAM_COST, item.kind, PROTOCOL_INTERFACES) });
                }
            }
            let need = ram_estimate(RAM_COST, ram_base_now, set, []) + STACK_RESERVE;
            if need > ram_limit { " — НЕ ПОМЕЩАЕТСЯ в RAM (оценка " + kib(need) + " из " + kib(ram_limit) + ")" } else { "" }
        };
        let marked = checklist("transports: отметьте транспорты (Enter — отметить или снять)",
            items.map(|item| item.value), items.map(|item| item.text), ram_note);
        for item in items {
            if !marked.contains(item.value) { continue; }
            let name = if item.value == item.kind { item.kind } else { item.value + " (" + item.kind.to_upper() + ")" };
            wanted.push(#{ kind: item.kind, name: name });
        }
    }

    for item in wanted {
        let kind = item.kind;
        let free = free_blocks(kind, INTERFACE_BLOCKS[kind], links, have, instances);
        // Протокол этого транспорта: пока протокол проекта не выбран — всё, что
        // работает поверх вида; когда выбран — он и `none`, а если поверх вида он
        // не работает, вопроса нет.
        let link_protocol = "none";
        if wanted_protocol != "" {
            link_protocol = wanted_protocol;
            if !PROTOCOL_INTERFACES[link_protocol].contains(kind) {
                throw "interface: протокол " + link_protocol + " работает поверх " + listed(PROTOCOL_INTERFACES[link_protocol]) + ", а не «" + kind + "»";
            }
        } else {
            let values = ["none"];
            let texts = ["none — без протокола: заглушка транспорта и его узел есть, протокол пишете вы"];
            for name in ["modbus", "protobuf", "canopen"] {
                if (protocol == "none" || protocol == name) && PROTOCOL_INTERFACES[name].contains(kind) {
                    values.push(name);
                    texts.push(PROTOCOL_TEXT[name]);
                }
            }
            if values.len() > 1 {
                link_protocol = ask("protocol: протокол связи поверх " + item.name + " (узел в графе)", values, texts, "none");
            }
        }
        links.push(#{ kind: kind, tag: free[0].tag, block: free[0].block, protocol: link_protocol });
        if protocol == "none" && link_protocol != "none" {
            protocol = link_protocol;
            if protocol == "modbus" { wants_master = modbus_master_answer(); }
        }
    }
    if links.len() == 0 && wanted_protocol != "" && wanted_protocol != "none" {
        throw "protocol: протокол " + wanted_protocol + " нужен транспорт — задайте interface (" + listed(PROTOCOL_INTERFACES[wanted_protocol]) + ")";
    }
}
if protocol != "modbus" && variable::is_set("modbus_master") {
    let answer = variable::get("modbus_master");
    if answer == "yes" || answer == "true" {
        throw "modbus_master: master есть только у протокола modbus, а выбран " + protocol;
    }
}

// --- Сетевые службы (`net_services`, `NET_SERVICE_ITEMS`): список с отметками,
// когда среди транспортов есть Ethernet; `--define net_services=ssh,rest` —
// строго, как `links`. Затем у SSH — способ входа (`ssh_auth`) и при входе по
// ключу — открытый ключ (`ssh_key`).
let has_eth = links.some(|link| link.kind == "tcp" || link.kind == "udp");
let services = [];
let service_items = NET_SERVICE_ITEMS.filter(|item| !item.needs_rng || rng[0] != "");
if variable::is_set("net_services") {
    for name in variable::get("net_services").split(",") {
        name.trim();
        if name == "" { continue; }
        let known = NET_SERVICE_ITEMS.filter(|item| item.value == name);
        if known.len() == 0 {
            throw "net_services: ожидается " + listed(NET_SERVICE_ITEMS.map(|item| item.value)) + ", а не «" + name + "»";
        }
        if !has_eth {
            throw "net_services: служба " + name + " работает поверх Ethernet — добавьте транспорт tcp или udp";
        }
        if known[0].needs_rng && rng[0] == "" {
            throw "net_services: у " + chip_name + " нет RNG, а " + name + " без случайных чисел небезопасен";
        }
        if !services.contains(name) { services.push(name); }
    }
} else if has_eth {
    if service_items.len() < NET_SERVICE_ITEMS.len() {
        print("SSH не предлагается: у " + chip_name + " нет RNG, а криптографии SSH нужны случайные числа");
    }
    services = checklist("net_services: отметьте сетевые службы (Enter — отметить или снять)",
        service_items.map(|item| item.value), service_items.map(|item| item.text));
}
let ssh_auth = "";
let ssh_key = "";
if services.contains("ssh") {
    ssh_auth = if variable::is_set("ssh_auth") {
        variable::get("ssh_auth")
    } else {
        // Умолчание (и ответ без терминала) — пароль: вход по ключу без
        // терминала было бы нечем закончить, а вопрос, который некому задать,
        // генерацию не роняет.
        ask("ssh_auth: вход в консоль SSH", ["password", "key"],
            ["password — по паролю", "key — по открытому ключу ed25519"], "password")
    };
    if ssh_auth != "key" && ssh_auth != "password" {
        throw "ssh_auth: ожидается key или password, а не «" + ssh_auth + "»";
    }
    if ssh_auth == "key" {
        // Пустой ввод — передумали: вход по паролю. Без терминала ввода нет
        // вовсе, и `ssh_auth=key` из `--define` без ключа — ошибка ниже.
        let declined = false;
        if variable::is_set("ssh_key") {
            ssh_key = variable::get("ssh_key");
        } else {
            try {
                ssh_key = variable::prompt("ssh_key: открытый ключ — строка ssh-ed25519 AAAA… из ~/.ssh/id_ed25519.pub (пусто — вход по паролю)",
                    "", "^$|^ssh-ed25519 " + SSH_KEY_BLOB_PREFIX + "[A-Za-z0-9+/]{43}( .*)?$");
                ssh_key.trim();
                declined = ssh_key == "";
            } catch (err) {
                if !err.to_string().contains("not a terminal") { throw err; }
            }
        }
        ssh_key.trim();
        if declined {
            ssh_auth = "password";
            print("Ключ не введён — вход в консоль SSH по паролю");
        } else if !ssh_key_ok(ssh_key, SSH_KEY_BLOB_PREFIX, BASE64_CHARS) {
            throw "ssh_key: вход по ключу — нужна строка открытого ключа ed25519 («ssh-ed25519 "
                + SSH_KEY_BLOB_PREFIX + "…», без кавычек); задайте --define ssh_key=\"…\" или ssh_auth=password";
        }
    }
}
for name in ["ssh_auth", "ssh_key"] {
    if !services.contains("ssh") && variable::is_set(name) {
        throw name + ": задаётся только вместе со службой ssh (net_services)";
    }
}
if ssh_auth == "password" && variable::is_set("ssh_key") {
    throw "ssh_key: ключ нужен только при ssh_auth=key";
}
variable::set("net_services", services.reduce(|sum, name| if sum == "" { name } else { sum + "," + name }, ""));
variable::set("ssh_auth", ssh_auth);
variable::set("ssh_key", ssh_key);

// --- RAM. Оценка (`ram_estimate`, байт статической RAM) — для сводки и для
// `template-check`, который сверяет её с фактом сборки. Не помещается проект с
// транспортами: из `--define` — ошибка, из меню — предупреждение (пункт был
// помечен). Проект без транспортов не отвергается: базу не выбирают.
variable::set("ram_estimate", "");
if ram_limit != () {
    let need = ram_estimate(RAM_COST, ram_base_now, links, services);
    variable::set("ram_estimate", need.to_string());
    print("ram_estimate: " + need + " байт статической RAM (+ " + STACK_RESERVE + " под стек) из " + ram_limit + " байт RAM");
    if links.len() > 0 && need + STACK_RESERVE > ram_limit {
        let why = "не помещается в RAM " + chip_name + " (оценка " + kib(need + STACK_RESERVE) + " из " + kib(ram_limit) + ")";
        if variable::is_set("links") { throw "links: " + variable::get("links") + " " + why; }
        if variable::is_set("interface") { throw "interface: " + variable::get("interface") + " с протоколом " + protocol + " " + why; }
        print("Внимание: выбранные транспорты " + why + " — прошивка может не собраться или упасть на переполнении стека");
    }
}

// Основной транспорт — первый с протоколом, а без протокола — первый. От него
// считаются прежние скаляры `interface`, `protocol`, `iface_periph`, `can_kind`,
// `usb_kind`. Исходники шаблона и сводка `post-script.rhai` на них уже не
// смотрят — идут циклом по `links` и признакам `has_*`; читает их только
// документация (README.md, AGENTS.md, docs/**), описывающая один транспорт.
// Производные флаги и удаление файлов ниже считаются по всем транспортам.
let primary = #{ kind: "none" };
for link in links {
    if link.protocol != "none" {
        primary = link;
        break;
    }
}
if primary.kind == "none" && links.len() > 0 { primary = links[0]; }
iface = primary.kind;

// Итог ответов: по нему считается раскладка. Если выбранное на чип не
// помещается, память остаётся человеку — как при `--define write_size=...`.
const WANTS_FINAL = #{ ota: wants_ota, ext_ota: wants_ext_ota, config: wants_config, external: wants_external, graph: wants_graph };
let plan_final = plan(layout, erase_zero, WANTS_FINAL, LIMITS);
if !variable::is_set("write_size") && ((wants_ota && !plan_final.ota) || (wants_config && !plan_final.config)) {
    variable::set("write_size", layout["write_size"]);
    print("Выбранное не помещается на " + chip_name + ": раскладку памяти (memory.x и числа в [env]");
    print("  crates-cross/.cargo/config.toml) вы заполняете сами — так же, как при --define write_size=...");
}

// Где лежат настройки (`config_store`) — одним решением для всех трёх веток
// ниже (см. `config_store()` выше).
let store = config_store(MEMORY_LAYOUT[suffix], wants_config, wants_external, EEPROM_MIN);

// write_size, оба memory.x и признак ota — вместе, одним источником: если
// пользователь уже задал write_size вручную через `--define`, авточипу
// доверять нельзя (адреса в memory.x привязаны именно к вычисленному
// write_size), оставляем всё как есть — ручное заполнение, как раньше.
// Тогда же остаётся и ota = "true": bootloader из проекта не исчезает,
// раскладку под него пользователь пишет сам.
// Числа раскладки для `[env]` (crates-cross/.cargo/config.toml): пустые, пока
// их не посчитала раскладка ниже. При ручной раскладке (`write_size`) их пишет
// человек, а в `[env]` пустая строка не выводится.
variable::set("active_len", "");
variable::set("config_start", "");
variable::set("config_end", "");
variable::set("state_erase_size", "");
// Переменные, которые ниже задаются только в части веток: Liquid не должен
// встречать неопределённую, даже если ветку с её чтением он не рисует.
variable::set("config_erase_size", "");
variable::set("ext_page_size", "");
variable::set("iface_periph", "");
// Регион `DMA_NC` (хвост RAM) под буферы DMA: границы числами, для `[env]`.
// Пусто — региона нет (раскладка ручная, ядро не M7).
variable::set("dma_nc_start", "");
variable::set("dma_nc_len", "");
// То же для `SRAM4` целиком под буферы BDMA (H7).
variable::set("bdma_nc_start", "");
variable::set("bdma_nc_len", "");
variable::set("can_kind", "");
variable::set("usb_kind", "");
// Список транспортов и признаки видов; итог — после ответов, рядом с `interface`.
variable::set("links", "");
for kind in LINK_KINDS { variable::set("has_" + kind, "false"); }
variable::set("has_net", "false");
// Нужна ли `STATE` и `CONFIG` своя страница (`flash_page::ErasePage`): только
// когда сектор раздела мельче страницы цельного `Flash` — F2/F4/F7. Иначе раздел
// строится прямо над `Whole`, а равенство страниц держит `const`-assert в `bsp`.
variable::set("state_paged", "false");
variable::set("config_paged", "false");
// Строит ли bootloader хоть один раздел над `BlockingPartition` + `ErasePage`
// (`STATE` со своей страницей или `ACTIVE`/`DFU` со страницей обмена): только
// тогда ему нужны `FLASH_BYTES` и обёртки; иначе все три раздела — прямо
// над `Flash`.
variable::set("boot_partitioned", "false");
if variable::is_set("write_size") {
    variable::set("ota", if wants_ota { "true" } else { "false" });
    variable::set("ota_store", if wants_ext_ota { "external" } else { "internal" });
    // Разкладку здесь пишет человек: DFU через регион банка 2 включает он сам.
    variable::set("dfu_bank", "false");
    if !wants_ota {
        file::delete("crates-cross/boot");
        file::delete("crates-cross/bsp/src/ota.rs");
        file::delete("crates-cross/app/src/ota.rs");
        file::delete("docs/modules/bsp-ota.md");
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
            print("  добавьте регион CONFIG вручную и впишите CONFIG_START/CONFIG_END в [env]");
        }
        file::delete("crates-cross/bsp/src/settings.rs");
        file::delete("crates-cross/app/src/settings.rs");
    }
    // Общий объект `Flash` в bsp нужен OTA во внутреннем flash и настройкам во
    // флеше или EEPROM; внешней флеши он не нужен (см. ту же переменную ниже).
    variable::set("shared_flash", if (wants_ota && !wants_ext_ota) || (wants_config && store == "eeprom") { "true" } else { "false" });
    // Модуль флеша в bsp нужен, пока есть общий замок; без него
    // файла и его документации быть не должно (`pub mod flash;` закрыт тем же
    // условием в lib.rs).
    if !(wants_ota && !wants_ext_ota) && !(wants_config && store == "eeprom") {
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
    // `ACTIVE`/`DFU` без раскладки не оборачиваются в `ErasePage`: страница обмена
    // здесь — страница цельного `Flash` (как было до расширения страницы).
    variable::set("swap_paged", "false");
    // Страница раздела `BOOTLOADER_STATE` (`flash_page::ErasePage`; число
    // уезжает переменной среды в crates-cross/.cargo/config.toml, её читают
    // bsp::flash и boot) — тоже знание раскладки. Её здесь нет,
    // поэтому — страница цельного `Flash`, как было до `ErasePage`: верно на
    // чипах с равными секторами, а на F2/F4/F7 `STATE` в мелком секторе
    // приведёт к панике `Partition::new` при старте — об этом печатается
    // предупреждение ниже.
    variable::set("state_erase_size", "");
    // Страницы сектора `STATE` здесь никто не знает — раздел остаётся под
    // `ErasePage` с числом, которое впишет человек.
    variable::set("state_paged", if wants_ota && !wants_ext_ota { "true" } else { "false" });
    variable::set("boot_partitioned", if wants_ota && !wants_ext_ota { "true" } else { "false" });
    if wants_ota && !wants_ext_ota {
        print("OTA с ручной раскладкой: впишите в [env] crates-cross/.cargo/config.toml числа, которые");
        print("  генерация считает сама: STATE_ERASE_SIZE (страница сектора STATE) и ACTIVE_LEN (размер ACTIVE");
        print("  в байтах) — без них проект не соберётся");
    }
    if wants_ota && wants_ext_ota {
        print("OTA с ручной раскладкой: впишите в [env] crates-cross/.cargo/config.toml ACTIVE_LEN (размер ACTIVE в байтах)");
    }
    if wants_config && store != "eeprom" {
        print("Хранилище настроек с ручной раскладкой: впишите в [env] CONFIG_START и CONFIG_END (границы раздела в байтах)");
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

    // Кэш данных ядра M7 включается только вместе с non-cacheable регионом под DMA
    // (`bsp::dma_nc`): без него буфер периферии читался бы из кэша устаревшим.
    // Регион есть у каждого M7 (F7, H7, H7R/S), кроме ядра CM4 двухъядерных H7;
    // откуда он взят — решает chip-data-gen. `SRAM4` под BDMA — только у H7.
    if variable::get("cpu") == "cortex-m7" {
        if m.contains("dma_nc_origin") {
            variable::set("dma_nc_start", hex_value(m["dma_nc_origin"]).to_string());
            variable::set("dma_nc_len", size_in_bytes(m["dma_nc_length"]).to_string());
        }
        if m.contains("bdma_nc_origin") {
            variable::set("bdma_nc_start", hex_value(m["bdma_nc_origin"]).to_string());
            variable::set("bdma_nc_len", size_in_bytes(m["bdma_nc_length"]).to_string());
        }
    }

    // Второй банк flash за разрывом (H743 с 1 MiB и т.п.): с внутренней OTA
    // DFU лежит в нём, и прошивка пишет его через регион банка.
    let dfu_bank = m.contains("dfu_bank_origin");

    let pl = plan(m, erase_zero, WANTS_FINAL, LIMITS);
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
        print("Настройки — на внешней флеши: подставьте свой драйвер в crates-cross/bsp/src/settings.rs");
        print("  (пока там заглушка bsp::stub::Flash — проект собирается, настройки не сохраняются)");
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
    // `flash_page::ErasePage<_, SWAP_PAGE_SIZE>` (= page_size). Без OTA страницы обмена нет —
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
        let state_paged = plain_number(m[ota_key + "bootloader_state_erase_size"]) != plain_number(m["page_size"]);
        variable::set("state_paged", if state_paged { "true" } else { "false" });
        variable::set("boot_partitioned", if state_paged || (swap_paged && !dfu_bank) { "true" } else { "false" });
    }
    // Шаг внешних разделов и страница обмена, которую раскладка ждёт от
    // драйвера: его `ERASE_SIZE` обязан её делить (const-assert рядом с ним).
    if ext_ota {
        variable::set("ext_page_size", m[ota_key + "page_size"]);
    }
    variable::set("ota", if ota { "true" } else { "false" });
    variable::set("ota_store", if ext_ota { "external" } else { "internal" });
    // Границы разделов приложению нужны числами, а не символами линкера:
    // `memory.x` считается этим же расчётом, и `unsafe extern` ради чтения
    // собственных констант ни к чему. Уезжают в `[env]` (crates-cross/.cargo/config.toml).
    if ota {
        variable::set("active_len", size_in_bytes(m[ota_key + "active_length"]).to_string());
    }
    if in_flash {
        // Смещения от базы flash: именно так их ждёт embassy-stm32::flash::Flash.
        let config_start = hex_value(m[settings_key + "origin"]) - hex_value(m["flash_origin"]);
        variable::set("config_start", config_start.to_string());
        variable::set("config_end", (config_start + size_in_bytes(m[settings_key + "length"])).to_string());
    } else if config && store == "external" && ext_ota {
        // Смещения от начала внешней флеши.
        let config_start = hex_value(m[ota_key + "config_offset"]);
        variable::set("config_start", config_start.to_string());
        variable::set("config_end", (config_start + size_in_bytes(m[ota_key + "config_length"])).to_string());
    }
    // DFU в банке 2 за разрывом: прошивка пишет его через `Bank2Region`, а не через
    // цельный `Flash` (см. `dfu_bank_origin` в chip-data-gen).
    variable::set("dfu_bank", if ota && !ext_ota && dfu_bank { "true" } else { "false" });
    variable::set("config", if config { "true" } else { "false" });
    variable::set("config_store", if config { store } else { "none" });
    if in_flash {
        variable::set("config_erase_size", m[settings_key + "erase_size"]);
        variable::set("config_paged", if plain_number(m[settings_key + "erase_size"]) != plain_number(m["page_size"]) { "true" } else { "false" });
    }
    // Общий объект `Flash` в bsp: его делят OTA во внутреннем flash и настройки
    // во флеше или EEPROM (EEPROM пишется через тот же контроллер). Внешней
    // флеши он не нужен — с `ota=external` приложение во внутренний flash не
    // пишет вовсе (`ACTIVE` пишет bootloader), — и без OTA создавать его
    // незачем: был бы неиспользуемый код.
    variable::set("shared_flash", if (ota && !ext_ota) || (config && store != "external") { "true" } else { "false" });
    if !((ota && !ext_ota) || (config && store != "external")) {
        file::delete("crates-cross/bsp/src/flash.rs");
        file::delete("docs/modules/bsp-flash.md");
    }
    variable::set("signed", if signed { "true" } else { "false" });
    variable::set("graph", if graph { "true" } else { "false" });
    if !config {
        file::delete("crates-cross/bsp/src/settings.rs");
        file::delete("crates-cross/app/src/settings.rs");
    }

    // База flash одна на оба режима: `__bootloader_*` отсчитываются от неё, а не
    // от ORIGIN(FLASH) конкретного крейта — так их читает embassy-boot.
    let flash_base = m["flash_origin"];
    let config_region = [];
    if in_flash {
        config_region.push("    CONFIG            (rx)  : ORIGIN = " + m[settings_key + "origin"] + ", LENGTH = " + m[settings_key + "length"]);
    }

    // RAM одинаков в обоих крейтах и в обоих режимах. Следом — все остальные
    // регионы чипа (ITCM, DTCM, CCMRAM, BKPSRAM, EEPROM, OTP, окна внешних
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
    // Секции под буферы DMA (`bsp::dma_nc`): статика с `#[link_section = ".dma_nc"]`
    // (`.bdma_nc` — для BDMA) ложится в non-cacheable регион и размечается в MPU.
    // Только в memory.x приложения — у bootloader'а кэшей нет. `INSERT AFTER
    // .gnu.sgstubs`, а не `.uninit`: вставка сразу за `.uninit` ломает проверку
    // стека в cortex-m-rt под flip-link (проверено сборкой).
    let dma_sections = [];
    if variable::get("dma_nc_start") != "" {
        dma_sections = [
            "",
            "/* Буферы DMA при включённом кэше данных ядра: bsp::dma_nc, docs/clocks.md. */",
            "SECTIONS",
            "{",
            "    .dma_nc (NOLOAD) : ALIGN(4)",
            "    {",
            "        *(.dma_nc .dma_nc.*);",
            "    } > DMA_NC",
        ];
        if variable::get("bdma_nc_start") != "" {
            dma_sections += [
                "",
                "    .bdma_nc (NOLOAD) : ALIGN(4)",
                "    {",
                "        *(.bdma_nc .bdma_nc.*);",
                "    } > SRAM4",
            ];
        }
        dma_sections += ["} INSERT AFTER .gnu.sgstubs;"];
    }
    if m.contains("extra_regions") {
        ram += m["extra_regions"];
    }
    if variable::get("dma_nc_start") != "" {
        ram.push("    /* DMA_NC - bsp::dma_nc держит его non-cacheable в MPU, из него берутся буферы DMA. */");
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
        app += panic_section;
        app += dma_sections;
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
        app += panic_section;
        app += dma_sections;
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
        file::delete("crates-cross/bsp/src/ota.rs");
        file::delete("crates-cross/app/src/ota.rs");
        file::delete("docs/modules/bsp-ota.md");
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
        single += panic_section;
        single += dma_sections;
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
	// Сторож запускает блок `watchdog:` графа; без графа его некому взводить, а
	// `Board` поля сторожа не имеет.
	file::delete("crates-cross/bsp/src/wdg.rs");
	file::delete("docs/modules/bsp-wdg.md");
} else {
	// С графом узлы спавнит он (`fragments:`), модули задач без графа не нужны.
	file::delete("crates-cross/app/src/tasks.rs");
	file::delete("crates-cross/app/src/tasks");
}

// Протокол и транспорт выбраны и проверены выше (вопросы после выбора чипа), а
// здесь остаётся подставить блок основного транспорта для групп `resources.rs`. Граф
// протоколу не нужен: без него узел спавнит `tasks::spawn_all` в `app` (как узел OTA).
if iface != "none" {
    variable::set("iface_periph", primary.block);
    if iface == "can" {
        variable::set("can_kind", primary.tag);
    }
    if iface == "usb" {
        variable::set("usb_kind", primary.tag);
    }
}
// `flash_page::ErasePage` и всё, что о нём сказано, нужны проекту только когда
// хоть один раздел его использует: `STATE`/`CONFIG` с секторами мельче страницы
// чипа или `ACTIVE`/`DFU` со страницей обмена. Иначе типа в проекте нет вовсе.
variable::set("erase_page", if variable::get("state_paged") == "true" || variable::get("config_paged") == "true" || variable::get("swap_paged") == "true" { "true" } else { "false" });
// Заглушка драйвера внешней флеши (`bsp::stub::Flash`) нужна, пока
// на внешней флеши лежит хоть что-то: разделы OTA или настройки.
variable::set("external_flash", if (variable::get("ota") == "true" && variable::get("ota_store") == "external") || variable::get("config_store") == "external" { "true" } else { "false" });
// Крейт `flash-page` (crates-host): `ErasePage` — при своей странице у раздела,
// `erase_size` — при OTA на внешней флеши (проверка раскладки в `bsp::external`).
variable::set("flash_page", if variable::get("erase_page") == "true" || (variable::get("ota") == "true" && variable::get("ota_store") == "external") { "true" } else { "false" });
variable::set("protocol", protocol);
variable::set("interface", iface);
// Все транспорты — по ним идут файлы шаблона: список и признаки видов.
variable::set("links", links_text(links));
let has = #{ uart: false, can: false, tcp: false, udp: false, usb: false };
for link in links { has[link.kind] = true; }
for kind in has.keys() {
    variable::set("has_" + kind, if has[kind] { "true" } else { "false" });
}
variable::set("has_net", if has.tcp || has.udp { "true" } else { "false" });
// Итог по кэшам: тег `scb_d` — кэш данных ядра M7 — только там, где есть регион
// `DMA_NC`. Файлы модулей, которых в проекте нет, уходят тут же.
if variable::get("dma_nc_start") != "" {
    cache += " scb_d";
} else {
    file::delete("crates-cross/bsp/src/dma_nc.rs");
}
variable::set("cache", cache);
if cache == "" {
    file::delete("crates-cross/bsp/src/cache.rs");
}

// Протоколы и их транспорты — по всем транспортам, а не по основному: «Modbus по
// UART и по TCP» требует и RTU, и MBAP. Поток байтов (UART, USB CDC, TCP), а не
// датаграмма: ему нужны `embedded-io-async`, `crc` и, под protobuf, COBS. UDP
// границу кадра получает от самой датаграммы, CAN — это вообще не поток.
let on = #{ modbus: false, protobuf: false, canopen: false };
let modbus_rtu = false;
let modbus_tcp = false;
let modbus_udp = false;
let cobs_used = false;
let sockets = 0;
for link in links {
    if link.protocol == "none" { continue; }
    on[link.protocol] = true;
    if link.protocol == "modbus" {
        if link.kind == "uart" || link.kind == "usb" { modbus_rtu = true; }
        if link.kind == "tcp" { modbus_tcp = true; }
        if link.kind == "udp" { modbus_udp = true; }
    }
    if link.protocol == "protobuf" && (link.kind == "uart" || link.kind == "usb" || link.kind == "tcp") {
        cobs_used = true;
    }
    if link.kind == "tcp" || link.kind == "udp" { sockets += 1; }
}
for name in on.keys() {
    variable::set("has_" + name, if on[name] { "true" } else { "false" });
}
let modbus_mbap = modbus_tcp || modbus_udp;
variable::set("modbus_rtu", if modbus_rtu { "true" } else { "false" });
variable::set("modbus_tcp", if modbus_tcp { "true" } else { "false" });
variable::set("modbus_udp", if modbus_udp { "true" } else { "false" });
variable::set("modbus_mbap", if modbus_mbap { "true" } else { "false" });
// Modbus поверх потока (RTU или TCP): чтение `embedded-io-async` и фейк потока
// в тестах адаптера; UDP-датаграммам они не нужны.
variable::set("modbus_stream", if modbus_rtu || modbus_tcp { "true" } else { "false" });
variable::set("cobs_used", if cobs_used { "true" } else { "false" });
// Сокеты протоколов на стеке `embassy-net` (порт, буферы): по одному на сетевой
// транспорт с протоколом. Транспорт без протокола сокета не открывает.
variable::set("protocol_sockets", sockets.to_string());
// Слоты сокетов в стеке (`SOCKETS` в `app::net`): по одному на каждый сетевой
// транспорт, с протоколом и без — на `tcp` без протокола сокет открывает
// приложение, и без слота под него `embassy-net` паникует.
let net_sockets = 0;
for link in links {
    if link.kind == "tcp" || link.kind == "udp" { net_sockets += 1; }
}
variable::set("net_sockets", net_sockets.to_string());

// Кадровый канал (`ports::FrameLink`, `LinkError`) нужен protobuf (канал хоста),
// шине CAN (ошибка `CanBus`) и UDP-каналу (`adapters::net::UdpLink` реализует
// `FrameLink`) — с протоколом и без него.
variable::set("link_port", if on.protobuf || has.can || has.udp { "true" } else { "false" });
// Порт хранилища настроек (`ports::SettingsStorage`) нужен, пока есть что в него
// класть: раздел, EEPROM или внешняя флеш (`config`) либо заглушка `Unavailable`
// у узла CANopen без хранилища. Иначе порта и адаптера в проекте нет.
variable::set("settings_port", if variable::get("config") == "true" || on.canopen { "true" } else { "false" });
variable::set("stream_io", if has.uart || has.usb || has.tcp { "true" } else { "false" });
// Заглушки периферии в `bsp/src/stub.rs`: UART, CAN, USB, Ethernet, внешняя флеш.
variable::set("has_stub", if has.uart || has.can || has.usb || has.tcp || has.udp || variable::get("external_flash") == "true" { "true" } else { "false" });
// CRC кадра: RTU (Modbus по UART и USB) и COBS (protobuf по потоку).
variable::set("crc_used", if modbus_rtu || cobs_used { "true" } else { "false" });
// Property-тесты кадрирования есть у Modbus (RTU, TCP, UDP) и у COBS.
variable::set("framing_tests", if on.modbus || cobs_used { "true" } else { "false" });
// Асинхронные трейты в `ports`: без них `#![expect(async_fn_in_trait)]` не
// срабатывает, и сборка падает.
variable::set("ports_async", if variable::get("ota") == "true" || variable::get("settings_port") == "true" || on.modbus || on.protobuf || on.canopen || has.can || has.udp { "true" } else { "false" });
// Модуль `link.rs` в `app`: транспорты протоколов (`app::link::<имя>`) — кадрирование,
// буферы и сокет над объектами платы. Без Modbus и protobuf транспорту нечего оборачивать (CANopen берёт
// шину CAN как есть).
variable::set("protocol_link", if on.modbus || on.protobuf { "true" } else { "false" });
// Группы `resources.rs` нужны, пока есть что в них класть: общий флеш, сторож
// графа или блоки транспортов. Иначе в файле остаётся пустая группа.
variable::set("has_resources", if links.len() > 0 || variable::get("shared_flash") == "true" || variable::get("graph") == "true" { "true" } else { "false" });
// Master Modbus: ответ на вопрос выше (только при protocol=modbus).
let master = on.modbus && wants_master;
variable::set("modbus_master", if master { "true" } else { "false" });
// Файл master'а — только там, где он выбран; `pub mod master;`, порт и
// клиентские impl'ы вырезает Liquid в комментариях. Без modbus каталог
// `domain/src/modbus` уже удалён ниже целиком, второй раз его не трогаем.
if on.modbus && !master {
	file::delete("crates-host/domain/src/modbus/master.rs");
}
// Модуль заглушки UART в bsp — только с транспортом uart: без него
// `bsp::serial` не объявлен, и файл остался бы мусором с Liquid внутри.
if !has.uart {
	file::delete("crates-cross/bsp/src/serial.rs");
	file::delete("docs/modules/bsp-serial.md");
}
// То же для CAN: модуль шины и его документация — только с транспортом can.
if !has.can {
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
if !on.modbus {
	file::delete("crates-host/ports/src/modbus.rs");
	file::delete("crates-host/ports/src/modbus");
	file::delete("crates-host/adapters/src/modbus.rs");
	file::delete("crates-host/adapters/src/modbus");
	file::delete("crates-host/domain/src/modbus.rs");
	file::delete("crates-host/domain/src/modbus");
	file::delete("crates-cross/app/src/modbus.rs");
}
// Транспорты Modbus и сокеты сети — только под выбранные транспорты: остальные
// файлы адаптеров были бы кодом, до которого не дотянуться. `mod`, `pub use` и
// тесты в `modbus.rs`/`net.rs` вырезает Liquid в комментариях.
if on.modbus {
	if !modbus_rtu {
		file::delete("crates-host/adapters/src/modbus/rtu.rs");
	}
	if !modbus_tcp {
		file::delete("crates-host/adapters/src/modbus/tcp.rs");
	}
	if !modbus_udp {
		file::delete("crates-host/adapters/src/modbus/udp.rs");
	}
	if !modbus_mbap {
		file::delete("crates-host/adapters/src/modbus/mbap.rs");
	}
}
if !has.tcp {
	file::delete("crates-host/adapters/src/net/tcp.rs");
}
if !has.udp {
	file::delete("crates-host/adapters/src/net/udp.rs");
}
if !cobs_used {
	file::delete("crates-host/adapters/src/cobs.rs");
}
if !on.protobuf {
	file::delete("crates-host/ports/src/host.rs");
	file::delete("crates-host/domain/src/host.rs");
	file::delete("crates-host/domain/src/host");
	file::delete("crates-host/proto");
}
if !has.can {
	file::delete("crates-host/ports/src/can.rs");
}
if !on.canopen {
	file::delete("crates-host/domain/src/canopen.rs");
	file::delete("crates-host/domain/src/canopen");
	file::delete("crates-host/canopen");
}
if variable::get("protocol_link") != "true" {
	file::delete("crates-cross/app/src/link.rs");
}
if variable::get("link_port") != "true" {
	file::delete("crates-host/ports/src/link.rs");
}
// OTA — порты, адаптер и узел `domain::ota` — только в проекте с OTA: без неё это
// код, до которого не дотянуться от `main` и графа. `mod`, `pub use` и зависимости
// вырезает Liquid.
if variable::get("ota") != "true" {
	file::delete("crates-host/ports/src/ota.rs");
	file::delete("crates-host/ports/src/ota");
	file::delete("crates-host/adapters/src/ota.rs");
	file::delete("crates-host/domain/src/ota.rs");
	file::delete("crates-host/domain/src/ota");
	// Перевод команд OTA у протокола: каталог протокола есть, только если он
	// выбран, — чужой хук уже удалил выше.
	if on.modbus {
		file::delete("crates-host/domain/src/modbus/ota.rs");
	}
	if on.canopen {
		file::delete("crates-host/domain/src/canopen/ota.rs");
	}
}
// То же для хранилища настроек.
if variable::get("ota") != "true" && !on.canopen {
	file::delete("crates-host/ports/src/word.rs");
}
if variable::get("settings_port") != "true" {
	file::delete("crates-host/ports/src/settings.rs");
	file::delete("crates-host/adapters/src/settings.rs");
}
// Мост blocking → async и заглушка внешней флеши — только когда ими кто-то
// пользуется: обёртка страницы (`erase_page`) или внешняя флеш.
if variable::get("flash_page") != "true" {
	file::delete("crates-host/flash-page");
}
// Модули задач без графа (`app/src/tasks/`) — по узлу, только выбранные.
if variable::get("graph") != "true" {
	if variable::get("ota") != "true" {
		file::delete("crates-cross/app/src/tasks/ota.rs");
	}
	if !has.tcp && !has.udp {
		file::delete("crates-cross/app/src/tasks/net.rs");
	}
	if !has.usb {
		file::delete("crates-cross/app/src/tasks/usb.rs");
	}
	if !on.modbus {
		file::delete("crates-cross/app/src/tasks/modbus.rs");
	}
	if !on.protobuf {
		file::delete("crates-cross/app/src/tasks/host.rs");
	}
	if !on.canopen {
		file::delete("crates-cross/app/src/tasks/canopen.rs");
	}
}
if variable::get("has_stub") != "true" {
	file::delete("crates-cross/bsp/src/stub.rs");
}
if !has.usb && !has.tcp && !has.udp {
	file::delete("crates-host/adapters/src/fake.rs");
}
// USB — устройство `embassy-usb`, узел USB и поток CDC — только с транспортом usb.
if !has.usb {
	file::delete("crates-cross/bsp/src/usb.rs");
	file::delete("crates-cross/app/src/usb.rs");
	file::delete("docs/modules/bsp-usb.md");
	file::delete("crates-host/adapters/src/usb.rs");
	file::delete("crates-host/domain/src/usb.rs");
}
// Сеть — стек `embassy-net`, узел NET и их сокеты — только с tcp/udp.
if !has.tcp && !has.udp {
	file::delete("crates-cross/bsp/src/net.rs");
	file::delete("crates-cross/app/src/net.rs");
	file::delete("docs/modules/bsp-net.md");
	file::delete("crates-host/adapters/src/net.rs");
	file::delete("crates-host/adapters/src/net");
	file::delete("crates-host/domain/src/net.rs");
}
// RNG — только с tcp/udp и только если он есть у чипа (`rng_irq` не пуст).
if rng[0] == "" || (!has.tcp && !has.udp) {
	file::delete("crates-cross/bsp/src/rng.rs");
	file::delete("docs/modules/bsp-rng.md");
}

// Обёртка EEPROM — только когда настройки лежат в ней; без неё файл с
// тестами и `pub mod eeprom;` (закрыт тем же условием в lib.rs) — мусор.
if variable::get("config_store") != "eeprom" {
	file::delete("crates-host/adapters/src/eeprom.rs");
	file::delete("crates-host/ports/src/eeprom.rs");
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
