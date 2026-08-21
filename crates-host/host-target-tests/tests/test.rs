//! Тесты, где хост управляет уже прошитым устройством
//! (`cargo xtask test host-target`).
//!
//! Третий этап трёхэтапной модели. Отличие от двух других — в том, кто кем
//! командует: `cargo xtask test host` гоняет логику `domain` на хосте без
//! всякого железа, `cargo xtask test target` выполняет код *внутри* МК, а
//! здесь тест — обычная host-программа, которая смотрит на устройство
//! снаружи и общается с ним так же, как это будет делать реальный оператор
//! или соседний сервис.
//!
//! Прошивку заливает `cargo xtask test host-target` (release-профиль,
//! bootloader + приложение) и передаёт сюда через окружение всё, что зависит
//! от чипа: `HOST_TARGET_CHIP`, адрес региона `PERSIST` и — если в проекте
//! есть OTA — границы разделов `ACTIVE`/`DFU`/`BOOTLOADER_STATE`, вершину RAM
//! и размер слова записи флеша. Через окружение, а не константами в коде: чип
//! подставляется при генерации в одном месте, а адреса считаются по
//! `memory.x`, то есть по той же раскладке, с которой собрана прошивка.
//!
//! Внешних зависимостей у крейта нет намеренно: `probe-rs` для этого этапа и
//! так обязателен (им же прошивают), и вызвать его как процесс дешевле, чем
//! тянуть в проект его библиотечную версию со своим графом зависимостей.
//!
//! Почему проверка идёт через чтение памяти, а не через defmt-лог: у
//! `probe-rs attach` нет режима «прочитать и выйти», он держит сессию, пока
//! его не убьют. А убитый на Windows `probe-rs` оставляет ST-Link
//! захваченным — следующая команда падает с `reset not supported by WinUSB`
//! до переподключения кабеля (поймано вживую на STM32F3Discovery). Поэтому
//! здесь только одноразовые команды, каждая из которых завершается сама.
//!
//! Когда у платы появится собственный канал связи (USB CDC, UART-протокол,
//! сеть), настоящие сценарии стоит писать поверх него — этот тест останется
//! проверкой того, что устройство живо и его прошивка исполняется.

use std::{
    env, fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

/// Магия, которую приложение кладёт в начало `PERSIST` (см. `PERSIST_MAGIC` в
/// crates-cross/app/src/main.rs). Совпадение с ней и означает «прошивка дошла до
/// `main` и успела поработать».
const PERSIST_MAGIC: u32 = 0xB007_C0DE;

/// Сколько ждать после сброса, прежде чем читать счётчик. Приложению нужно
/// дойти до `main`, то есть отработать инициализацию HAL; на порядок больше
/// того, что это занимает на практике, но всё ещё мгновение по меркам теста.
const BOOT_TIME: Duration = Duration::from_millis(300);

#[test]
fn firmware_runs_and_counts_boots_across_resets() {
    let chip = required_var("HOST_TARGET_CHIP");
    let persist = required_var("HOST_TARGET_PERSIST_ADDR");

    // Пауза и перед первым чтением, не только после своего сброса: сюда
    // попадают сразу за `cargo flash`, который заканчивается сбросом платы, а
    // на тёплом кеше nextest стартует мгновенно — без задержки чтение обгоняет
    // `main` и тест мигает «прошивка не дошла до main».
    thread::sleep(BOOT_TIME);

    let (magic, first) = read_persist(&chip, &persist);
    assert_eq!(
        magic, PERSIST_MAGIC,
        "в начале PERSIST не магия приложения, а {magic:#010x} — прошивка не дошла до main \
         (или залита не она)"
    );

    reset(&chip);
    thread::sleep(BOOT_TIME);

    let (magic, second) = read_persist(&chip, &persist);
    assert_eq!(
        magic, PERSIST_MAGIC,
        "после сброса магия в PERSIST пропала: {magic:#010x}"
    );
    assert_eq!(
        second,
        first + 1,
        "счётчик запусков не вырос на сбросе: было {first}, стало {second} — либо прошивка \
         не стартовала, либо PERSIST не пережил сброс"
    );
}

/// Первые два слова региона: магия и счётчик запусков.
fn read_persist(chip: &str, address: &str) -> (u32, u32) {
    let output = probe_rs(&["read", "--chip", chip, "b32", address, "2"]);
    let mut words = output.split_whitespace().map(|word| {
        u32::from_str_radix(word, 16)
            .unwrap_or_else(|_| panic!("`probe-rs read` вернул не hex-слово: {word:?}"))
    });
    let magic = words.next().expect("probe-rs read не вернул ни слова");
    let count = words
        .next()
        .expect("probe-rs read вернул только одно слово");
    (magic, count)
}

fn reset(chip: &str) {
    probe_rs(&["reset", "--chip", chip]);
}

fn probe_rs(args: &[&str]) -> String {
    let output = Command::new("probe-rs")
        .args(args)
        .output()
        .expect("не удалось запустить probe-rs — установите его: cargo xtask setup");
    assert!(
        output.status.success(),
        "`probe-rs {}` завершилась с {}:\n{}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn required_var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} не задана: этот этап запускается через `cargo xtask test host-target`, \
             который сам прошивает плату и передаёт сюда чип и адрес PERSIST"
        )
    })
}

/// Сколько ждать смены разделов. Bootloader переносит партиции постранично, и
/// на чипе с крупными секторами это заметные секунды; ждём не фиксированное
/// время, а нужного содержимого (см. [`wait_for_active`]) — этот предел лишь
/// ограничивает ожидание сверху.
const SWAP_TIMEOUT: Duration = Duration::from_secs(20);

/// Сколько слов образа сравнивать. Первые два — начальный указатель стека и
/// адрес обработчика сброса, дальше — таблица векторов: этого с запасом
/// хватает, чтобы отличить один образ от другого.
const HEAD_WORDS: usize = 4;

/// Магия «поменяй разделы на следующем сбросе» из `embassy-boot`
/// (`SWAP_MAGIC`, `lib.rs`). Состояние — это `WRITE_SIZE` байт одного и того
/// же значения в стёртом разделе; ровно это и делает `mark_updated()`.
const SWAP_MAGIC: u8 = 0xF0;

/// Магия «обновления нет, запускай что лежит» (`BOOT_MAGIC` там же). Ею тест
/// начинает — и это не перестраховка.
///
/// После УСПЕШНОГО прогона состояние безопасно: bootloader, откатив образ,
/// пишет туда `REVERT_MAGIC`, и следующий запуск ничего не откатывает. А вот
/// прогон, прерванный между обменом и откатом (упал по таймауту, снято
/// Ctrl+C, отвалился пробник), оставляет `SWAP_MAGIC` с завершённым журналом
/// — то есть «обмен сделан, подтверждения не было». Первый же сброс после
/// такого затрёт только что прошитый `ACTIVE` тем, что лежит в `DFU`.
const BOOT_MAGIC: u8 = 0xD0;

/// Значение стёртого флеша у STM32. Им заполняется остаток раздела состояния:
/// так `probe-rs download` сотрёт его целиком, оставив ровно то, что оставляет
/// `mark_updated()`.
const ERASED: u8 = 0xFF;

/// Полный цикл обновления: смена разделов на сбросе и откат образа, который
/// себя не подтвердил.
///
/// Это единственная проверка второй половины OTA. Тест на устройстве
/// (`cargo xtask test target`) доходит только до записи в `DFU` и намеренно
/// не зовёт `mark_updated`: он не переживёт собственный сброс, а раннер
/// перезаливает образ между тестами. Отсюда и хост: он умеет и записать
/// раздел, и сбросить плату, и посмотреть, что получилось.
///
/// Обновление подсовывается **инертное** — восемь байт таблицы векторов и
/// бесконечный цикл. Так и должно быть: образ обязан стартовать (иначе
/// проверялся бы не откат, а поведение при HardFault) и обязан НЕ дойти до
/// `mark_booted()`, иначе откатывать будет нечего. Настоящая прошивка не
/// годится ни для того, ни для другого — она себя подтверждает.
///
/// После теста плата остаётся на исходном образе: откат — часть проверки.
///
/// В проекте без OTA тест сам себя пропускает: адреса разделов приходят из
/// `memory.x`, а там их нет — один образ на весь flash. Пропуск сделан
/// проверкой окружения, а не Liquid-условием в этом файле, и это важно:
/// `host-target-tests` — член корневого workspace, то есть его исходник
/// компилируется и в самом репозитории шаблона, где условия ещё не
/// подставлены. Условие сломало бы там `cargo xtask lint` и `test host`.
#[test]
fn ota_swaps_partitions_and_reverts_unconfirmed_image() {
    let Ok(active) = env::var("HOST_TARGET_ACTIVE_ADDR") else {
        eprintln!("OTA в проекте нет: раздела DFU не существует, проверять нечего");
        return;
    };
    let chip = required_var("HOST_TARGET_CHIP");
    let active = parse_address(&active);
    let dfu = required_var("HOST_TARGET_DFU_ADDR");
    let state = required_var("HOST_TARGET_STATE_ADDR");
    let ram_end = parse_address(&required_var("HOST_TARGET_RAM_END"));
    let write_size: usize = required_var("HOST_TARGET_WRITE_SIZE")
        .parse()
        .expect("HOST_TARGET_WRITE_SIZE — число байт");
    let state_len: usize = required_var("HOST_TARGET_STATE_LEN")
        .parse()
        .expect("HOST_TARGET_STATE_LEN — число байт");
    // Десятичное, в отличие от адресов: размеры приходят из раскладки числом.
    let page_size: u32 = required_var("HOST_TARGET_PAGE_SIZE")
        .parse()
        .expect("HOST_TARGET_PAGE_SIZE — число байт");

    // Известное состояние перед началом: «обновления нет». После прошлого
    // прогона в разделе состояния лежит неподтверждённое обновление, и первый
    // же сброс увёл бы плату в откат — тест мерил бы не то, что думает.
    let state_path = env::temp_dir().join("host-target-ota-state.bin");
    write_state(&state_path, BOOT_MAGIC, write_size, state_len);
    download(&chip, &state, &state_path);
    reset(&chip);
    thread::sleep(BOOT_TIME);

    let active_hex = format!("{active:#x}");
    let original = read_words(&chip, &active_hex, HEAD_WORDS);

    // Инертный образ: стек на вершину RAM, обработчик сброса — сразу за
    // этими двумя словами, а там `b .`. Бит 0 в адресе обработчика — режим
    // Thumb, без него Cortex-M уходит в HardFault на первой же инструкции.
    let mut inert = Vec::new();
    inert.extend_from_slice(&ram_end.to_le_bytes());
    inert.extend_from_slice(&(active + 8 + 1).to_le_bytes());
    inert.extend_from_slice(&0xE7FE_u16.to_le_bytes());
    let inert_path = env::temp_dir().join("host-target-ota-image.bin");
    fs::write(&inert_path, &inert).expect("записать временный образ");

    download(&chip, &dfu, &inert_path);
    write_state(&state_path, SWAP_MAGIC, write_size, state_len);
    download(&chip, &state, &state_path);

    reset(&chip);
    let expected_head = [
        u32::from_le_bytes(inert[0..4].try_into().expect("слово")),
        u32::from_le_bytes(inert[4..8].try_into().expect("слово")),
    ];
    // Признак завершённого обмена — оба раздела сразу: в `ACTIVE` новый
    // образ, в `DFU` старый. Смотреть только на `ACTIVE` мало: bootloader
    // переносит страницы по одной, и его голова меняется задолго до конца
    // переноса — сброс в этот момент застал бы обмен на середине.
    //
    // Прежний образ ищется СО СДВИГОМ НА СТРАНИЦУ, и это не деталь
    // реализации, о которой можно забыть: `swap()` в embassy-boot переносит
    // `ACTIVE[i]` не в `DFU[i]`, а в `DFU[i + 1]` — лишняя страница `DFU`
    // (её требует `assert_partitions`) работает разменной. В начале `DFU`
    // при этом так и остаётся первая страница нового образа.
    let dfu_previous = format!("{:#x}", parse_address(&dfu) + page_size);
    let swapped = wait_for(|| {
        read_words(&chip, &active_hex, HEAD_WORDS)[..2] == expected_head
            && read_words(&chip, &dfu_previous, HEAD_WORDS) == original
    });
    assert!(
        swapped,
        "после сброса разделы не поменялись местами: в ACTIVE не залитый образ или в DFU не \
         прежний (магия SWAP не дошла до BOOTLOADER_STATE либо схема разделов не сходится)"
    );

    // Второй сброс: подтверждения не было (инертный образ ничего не делает),
    // значит bootloader обязан вернуть предыдущий. Здесь сдвига уже нет —
    // `revert()` переносит `ACTIVE[i]` в `DFU[i]` и читает новый образ для
    // `ACTIVE` из `DFU[i + 1]`, то есть оттуда, куда его положил обмен.
    reset(&chip);
    let reverted = wait_for(|| {
        read_words(&chip, &active_hex, HEAD_WORDS) == original
            && read_words(&chip, &dfu, HEAD_WORDS)[..2] == expected_head
    });
    assert!(
        reverted,
        "образ не откатился: в ACTIVE осталось не то, что было до обновления. Это значит, что \
         неработающая прошивка, проехавшая по OTA, оставила бы плату мёртвой"
    );
}

/// Ждёт выполнения условия, но не дольше [`SWAP_TIMEOUT`].
///
/// Опрос, а не фиксированная пауза: перенос разделов занимает от долей
/// секунды до нескольких (зависит от размера страницы и раздела), и любая
/// пауза «на глаз» была бы либо флаки, либо вдвое длиннее нужного.
fn wait_for(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + SWAP_TIMEOUT;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Готовит образ раздела состояния: магия в начале, дальше стёртый флеш до
/// конца раздела.
///
/// Раздел пишется целиком не для полноты картины: `mark_updated()` начинает с
/// erase ВСЕГО раздела, а `probe-rs download` стирает ровно те секторы, в
/// которые пишет. Ограничься тест первым словом — на чипе с мелкими
/// страницами (раздел состояния там многосекторный: журнал прогресса это
/// слово на каждую страницу `ACTIVE` в каждом из четырёх проходов) следующий
/// прогон работал бы поверх прошлого журнала.
fn write_state(path: &Path, magic: u8, write_size: usize, state_len: usize) {
    let mut image = vec![ERASED; state_len];
    image[..write_size].fill(magic);
    fs::write(path, &image).expect("записать образ раздела состояния");
}

/// Заливает сырой образ по абсолютному адресу. Стиранием секторов занимается
/// сам `probe-rs`.
fn download(chip: &str, address: &str, path: &Path) {
    let path = path.to_string_lossy().into_owned();
    probe_rs(&[
        "download",
        "--chip",
        chip,
        "--binary-format",
        "bin",
        "--base-address",
        address,
        &path,
    ]);
}

fn read_words(chip: &str, address: &str, words: usize) -> Vec<u32> {
    let count = words.to_string();
    probe_rs(&["read", "--chip", chip, "b32", address, &count])
        .split_whitespace()
        .map(|word| {
            u32::from_str_radix(word, 16)
                .unwrap_or_else(|_| panic!("`probe-rs read` вернул не hex-слово: {word:?}"))
        })
        .collect()
}

/// `0x08020000` из окружения — в число.
fn parse_address(raw: &str) -> u32 {
    let digits = raw.trim_start_matches("0x");
    u32::from_str_radix(digits, 16).unwrap_or_else(|_| panic!("не адрес: {raw}"))
}
