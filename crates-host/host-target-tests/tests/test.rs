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
//! от чипа: `HOST_TARGET_CHIP` и — если в проекте есть OTA — границы разделов
//! `ACTIVE`/`DFU`/`BOOTLOADER_STATE`, размер слова записи флеша, инертный
//! образ для `DFU` и (с подписью) запись о его подписи. Через окружение, а не
//! константами в коде: чип подставляется при генерации в одном месте, адреса
//! считаются по `memory.x`, то есть по той же раскладке, с которой собрана
//! прошивка, а подписать образ может только `xtask` — у него ключ.
//!
//! Внешних зависимостей у крейта нет намеренно: `probe-rs` для этого этапа и
//! так обязателен (им же прошивают), и вызвать его как процесс дешевле, чем
//! тянуть в проект его библиотечную версию со своим графом зависимостей.
//!
//! Почему проверка идёт через чтение памяти, а не через defmt-лог: у
//! `probe-rs attach` нет режима «прочитать и выйти», он держит сессию, пока
//! его не убьют. А убитый на Windows `probe-rs` оставляет ST-Link
//! захваченным — следующая команда падает с `reset not supported by WinUSB`
//! до переподключения кабеля (поймано вживую на `STM32F3Discovery`). Поэтому
//! здесь только одноразовые команды, каждая из которых завершается сама.
//!
//! В проекте без OTA этот этап пуст, и это честно: без собственного канала
//! связи (USB CDC, UART-протокол, сеть) хосту нечего спросить у устройства
//! снаружи. Появится канал — настоящие сценарии пишутся здесь, поверх него.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

/// Сколько ждать после сброса, прежде чем читать состояние платы. Приложению
/// нужно дойти до `main`, то есть отработать инициализацию HAL; на порядок
/// больше того, что это занимает на практике, но всё ещё мгновение по меркам
/// теста.
const BOOT_TIME: Duration = Duration::from_millis(300);

/// Нижняя граница ожидания обмена — на чипах с мелкими страницами весь
/// обмен укладывается в доли секунды, и время уходит на сброс и опрос.
const SWAP_TIMEOUT_BASE: Duration = Duration::from_secs(20);

/// Запас времени на стирание и программирование 128 KiB флеша. Худший
/// известный случай — F0/F1 с записью полусловами: 128 страниц по ≤ 40 мс и
/// 64K полуслов по ≤ 70 мкс (datasheet) — около 10 с. У H7 измерено ~1,2 с на
/// стирание сектора 128K, программирование словами по 32 байта на этом фоне
/// мало. Предел ограничивает только провал: успех ждёт содержимого, а не его.
const COPY_PER_128K: Duration = Duration::from_secs(10);

/// Сколько слов образа сравнивать. Первые два — начальный указатель стека и
/// адрес обработчика сброса, дальше — таблица векторов: этого с запасом
/// хватает, чтобы отличить один образ от другого.
const HEAD_WORDS: usize = 4;

/// Магия «поменяй разделы на следующем сбросе» из `embassy-boot`
/// (`SWAP_MAGIC`, `lib.rs`). Состояние — это `WRITE_SIZE` байт одного и того
/// же значения в стёртом разделе; ровно это и делает `mark_updated()`.
const SWAP_MAGIC: u8 = 0xF0;

/// Магия «откат сделан» (`REVERT_MAGIC` там же). Её пишет только `revert()`,
/// а зовётся он, только если обмен уже состоялся (`is_swapped`), — поэтому
/// она в разделе состояния доказывает весь цикл «обмен → откат» сама, как бы
/// ни легли по времени сбросы. Живёт недолго: прошивка, к которой откатились,
/// подтверждает себя при старте и пишет поверх `BOOT_MAGIC`.
const REVERT_MAGIC: u8 = 0xC0;

/// Магия «обновления нет, запускай что лежит» (`BOOT_MAGIC` там же). Ею тест
/// начинает — и это не перестраховка.
///
/// После УСПЕШНОГО прогона состояние безопасно: bootloader, откатив образ,
/// пишет туда `REVERT_MAGIC`, и следующий запуск ничего не откатывает. А вот
/// прогон, прерванный между обменом и откатом (упал по таймауту, снято
/// Ctrl+C, отвалился пробник), оставляет `SWAP_MAGIC` с завершённым журналом
/// — то есть «обмен сделан, подтверждения не было». Первый же сброс после
/// такого затрёт только что прошитый `ACTIVE` тем, что лежит в `DFU`.
///
/// Ею же bootloader с подписью снимает запрос на обмен, отвергнув образ
/// (`domain::ota::boot::screen_update`).
const BOOT_MAGIC: u8 = 0xD0;

/// Значение стёртого флеша у STM32. Им заполняется остаток раздела состояния:
/// так `probe-rs download` сотрёт его целиком, оставив ровно то, что оставляет
/// `mark_updated()`.
const ERASED: u8 = 0xFF;

/// Раскладка OTA из окружения — то, что нужно обоим тестам цикла.
struct Ota {
    chip: String,
    active: u32,
    active_len: u32,
    dfu: u32,
    state: String,
    state_len: usize,
    write_size: usize,
    page_size: u32,
    /// Инертный образ (см. `inert_image` в `xtask`) — путь и байты.
    image_path: PathBuf,
    image: Vec<u8>,
    /// Подписываются ли образы в этом проекте.
    signed: bool,
}

impl Ota {
    /// `None` — в проекте нет OTA: адреса разделов приходят из `memory.x`, а
    /// там их нет — один образ на весь flash. Пропуск сделан проверкой
    /// окружения, а не Liquid-условием в этом файле, и это важно:
    /// `host-target-tests` — член корневого workspace, то есть его исходник
    /// компилируется и в самом репозитории шаблона, где условия ещё не
    /// подставлены. Условие сломало бы там `cargo xtask lint` и `test host`.
    fn from_env() -> Option<Self> {
        let Ok(active) = env::var("HOST_TARGET_ACTIVE_ADDR") else {
            eprintln!("OTA в проекте нет: раздела DFU не существует, проверять нечего");
            return None;
        };
        let image_path = PathBuf::from(required_var("HOST_TARGET_INERT_IMAGE"));
        let image = fs::read(&image_path).expect("прочитать инертный образ от xtask");
        Some(Self {
            chip: required_var("HOST_TARGET_CHIP"),
            active: parse_address(&active),
            active_len: number("HOST_TARGET_ACTIVE_LEN"),
            dfu: parse_address(&required_var("HOST_TARGET_DFU_ADDR")),
            state: required_var("HOST_TARGET_STATE_ADDR"),
            state_len: number("HOST_TARGET_STATE_LEN"),
            write_size: number("HOST_TARGET_WRITE_SIZE"),
            // Десятичное, в отличие от адресов: размеры приходят из раскладки числом.
            page_size: number("HOST_TARGET_PAGE_SIZE"),
            image_path,
            image,
            signed: required_var("HOST_TARGET_SIGNED") == "true",
        })
    }

    /// Запись о подписи bootloader ищет по смещению `capacity` от начала
    /// `DFU`, а `capacity` у него — размер `ACTIVE` (`config.active.size()` в
    /// `boot/src/signature.rs`): запасная страница `DFU`.
    fn record_addr(&self) -> String {
        format!("{:#x}", self.dfu + self.active_len)
    }

    /// Известное состояние перед началом: «обновления нет». После прошлого
    /// прогона в разделе состояния лежит неподтверждённое обновление, и первый
    /// же сброс увёл бы плату в откат — тест мерил бы не то, что думает.
    fn start_clean(&self, state_path: &Path) {
        write_state(state_path, BOOT_MAGIC, self.write_size, self.state_len);
        download(&self.chip, &self.state, state_path);
        reset(&self.chip);
        thread::sleep(BOOT_TIME);
    }

    /// Магия в начале раздела состояния. Занимает одно слово записи — у F0/F1
    /// это два байта, дальше уже журнал, поэтому сравнивается не больше слова.
    fn state_is(&self, magic: u8) -> Result<bool, String> {
        let head = try_read_words(&self.chip, &self.state, 1)?[0].to_le_bytes();
        Ok(head[..self.write_size.min(4)].iter().all(|&b| b == magic))
    }

    /// Первые два слова инертного образа — стек и обработчик сброса.
    fn image_head(&self) -> [u32; 2] {
        [0, 4]
            .map(|at| u32::from_le_bytes(self.image[at..at + 4].try_into().expect("слово образа")))
    }
}

fn reset(chip: &str) {
    probe_rs(&["reset", "--chip", chip]);
}

/// Команда `probe-rs`, отказ которой — провал теста.
fn probe_rs(args: &[&str]) -> String {
    try_probe_rs(args).unwrap_or_else(|why| panic!("{why}"))
}

/// Команда `probe-rs`, отказ которой бывает временным: пока bootloader стирает
/// сектор банка, из которого сам исполняется, ядро стоит на выборке
/// инструкции, остановить его пробник не может, и `read` отвечает ошибкой
/// (`Core is running, but we expected it to be halted`; ~1,2 с на сектор 128K
/// у H7). Не запустился сам `probe-rs` — это не временно, паника.
fn try_probe_rs(args: &[&str]) -> Result<String, String> {
    let output = Command::new("probe-rs")
        .args(args)
        .output()
        .expect("не удалось запустить probe-rs — установите его: cargo xtask setup");
    if !output.status.success() {
        return Err(format!(
            "`probe-rs {}` завершилась с {}:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn required_var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} не задана: этот этап запускается через `cargo xtask test host-target`, \
             который сам прошивает плату и передаёт сюда чип и адреса разделов"
        )
    })
}

/// Сколько ждать смены разделов: [`SWAP_TIMEOUT_BASE`] плюс перенос страниц.
///
/// Обмен в `embassy-boot` стирает и переписывает каждую страницу `ACTIVE` и
/// каждую страницу `DFU` по разу — вдвое больше байт, чем в `ACTIVE`; откат —
/// столько же. Ждём не фиксированное время, а нужного содержимого (см.
/// [`wait_for`]) — предел лишь ограничивает ожидание сверху. Постоянный предел
/// на H7 (раздел из нескольких секторов по 128K) не уложил бы и один обмен.
fn swap_timeout(active_len: u32) -> Duration {
    SWAP_TIMEOUT_BASE + COPY_PER_128K * (2 * active_len).div_ceil(128 * 1024)
}

fn number<T: std::str::FromStr>(name: &str) -> T {
    required_var(name)
        .parse()
        .unwrap_or_else(|_| panic!("{name} — не число байт"))
}

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
/// проверялся бы не откат, а поведение при `HardFault`) и обязан НЕ дойти до
/// `mark_booted()`, иначе откатывать будет нечего. Настоящая прошивка не
/// годится ни для того, ни для другого — она себя подтверждает.
///
/// С подписью образ кладётся вместе с записью о ней (`ports::PendingImage`),
/// которую подписал `xtask`: без записи bootloader обмен не пустит — это
/// отдельный тест ниже. Закрытого ключа нет (CI, свежий клон) — подписать
/// нечем, и тест пропускается с предупреждением.
///
/// После теста плата остаётся на исходном образе: откат — часть проверки.
#[test]
fn ota_swaps_partitions_and_reverts_unconfirmed_image() {
    let Some(ota) = Ota::from_env() else {
        return;
    };
    let record = env::var_os("HOST_TARGET_SIGNATURE_RECORD").map(PathBuf::from);
    if ota.signed && record.is_none() {
        eprintln!(
            "ВНИМАНИЕ: обмен и откат НЕ проверены — образы подписываются, а закрытого ключа \
             нет, подписать инертный образ нечем"
        );
        return;
    }
    let chip = &ota.chip;
    let state_path = env::temp_dir().join("host-target-ota-state.bin");
    ota.start_clean(&state_path);

    let active_hex = format!("{:#x}", ota.active);
    let dfu_hex = format!("{:#x}", ota.dfu);
    let original = read_words(chip, &active_hex, HEAD_WORDS);

    download(chip, &dfu_hex, &ota.image_path);
    // Запись лежит в запасной странице `DFU`, отдельно от начала образа, —
    // отдельной заливкой.
    if let Some(record) = &record {
        download(chip, &ota.record_addr(), record);
    }
    write_state(&state_path, SWAP_MAGIC, ota.write_size, ota.state_len);
    download(chip, &ota.state, &state_path);

    reset(chip);
    let expected_head = ota.image_head();
    let timeout = swap_timeout(ota.active_len);
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
    //
    // В проекте с графом задач обмен можно и не застать: release-bootloader
    // запускает сторож, а инертный образ его не кормит и через
    // таймаут сторожа (10 с, `bsp::wdg::HW_TIMEOUT_US`) после прыжка сбрасывается сам — bootloader
    // тут же откатывает его. Тогда обмен доказывает магия `REVERT`: без
    // состоявшегося обмена отката не бывает.
    let dfu_previous = format!("{:#x}", ota.dfu + ota.page_size);
    let swapped = wait_for(timeout, || {
        Ok(
            (try_read_words(chip, &active_hex, HEAD_WORDS)?[..2] == expected_head
                && try_read_words(chip, &dfu_previous, HEAD_WORDS)? == original)
                || ota.state_is(REVERT_MAGIC)?,
        )
    });
    if let Err(why) = swapped {
        panic!(
            "после сброса разделы не поменялись местами: в ACTIVE не залитый образ или в DFU не \
             прежний (магия SWAP не дошла до BOOTLOADER_STATE, схема разделов не сходится либо, \
             с подписью, bootloader отверг запись). {why}"
        );
    }

    // Второй сброс: подтверждения не было (инертный образ ничего не делает),
    // значит bootloader обязан вернуть предыдущий. Если откат уже сделал
    // сторож (см. выше), сброс ничего не меняет: состояние `REVERT`, и
    // bootloader просто запускает прежний образ. Здесь сдвига уже нет —
    // `revert()` переносит `ACTIVE[i]` в `DFU[i]` и читает новый образ для
    // `ACTIVE` из `DFU[i + 1]`, то есть оттуда, куда его положил обмен.
    //
    // Состояние — `REVERT` ИЛИ `BOOT`: вернувшаяся прошивка при старте
    // подтверждает себя (`mark_booted` в `begin` узла OTA) и за миллисекунды
    // меняет `REVERT` на `BOOT` — опрос раз в полсекунды `REVERT` почти не
    // застаёт. Откат доказывают разделы, а состояние — лишь то, что нового
    // обмена не запрошено (не `SWAP`).
    reset(chip);
    let reverted = wait_for(timeout, || {
        Ok(try_read_words(chip, &active_hex, HEAD_WORDS)? == original
            && try_read_words(chip, &dfu_hex, HEAD_WORDS)?[..2] == expected_head
            && (ota.state_is(REVERT_MAGIC)? || ota.state_is(BOOT_MAGIC)?))
    });
    if let Err(why) = reverted {
        panic!(
            "образ не откатился: в ACTIVE осталось не то, что было до обновления. Это значит, \
             что неработающая прошивка, проехавшая по OTA, оставила бы плату мёртвой. {why}"
        );
    }
}

/// С подписью — образ без записи о ней bootloader не пускает: снимает запрос
/// на обмен (`BOOT_MAGIC` в состоянии), `ACTIVE` остаётся прежним. Это и есть
/// свойство подписанного OTA — чужой образ, положенный в `DFU` мимо
/// приложения, не встаёт. Без подписи проверять нечего, тест пропускается.
#[test]
fn ota_rejects_unsigned_image() {
    let Some(ota) = Ota::from_env() else {
        return;
    };
    if !ota.signed {
        eprintln!("образы не подписываются — отказывать неподписанному некому");
        return;
    }
    let chip = &ota.chip;
    let state_path = env::temp_dir().join("host-target-ota-state.bin");
    ota.start_clean(&state_path);

    let active_hex = format!("{:#x}", ota.active);
    let original = read_words(chip, &active_hex, HEAD_WORDS);

    // Запись от прошлого подписанного прогона, прерванного до обмена, лежала
    // бы там до сих пор и была бы верной — стираем явно. `probe-rs download`
    // стирает сектор целиком, записывается одно стёртое слово.
    let erased_path = env::temp_dir().join("host-target-ota-erased.bin");
    fs::write(&erased_path, vec![ERASED; ota.write_size]).expect("записать стёртое слово");
    download(chip, &ota.record_addr(), &erased_path);
    download(chip, &format!("{:#x}", ota.dfu), &ota.image_path);
    write_state(&state_path, SWAP_MAGIC, ota.write_size, ota.state_len);
    download(chip, &ota.state, &state_path);

    reset(chip);
    // `SWAP` в состоянии до проверки bootloader'ом, `BOOT` — после отказа:
    // между ними раздел стёрт, и пока он стирается, чтения отказывают.
    //
    // `BOOT` с прежним `ACTIVE` — ещё и конец ОШИБОЧНО пропущенного обмена в
    // проекте со сторожем: инертный образ встал, сторож сбросил, bootloader
    // откатил, прежняя прошивка подтвердила себя. Различает их сам инертный
    // образ: пропущенный, он сидит в `ACTIVE` до сброса сторожем (секунды,
    // `bsp::wdg::HW_TIMEOUT_US`) — опрос раз в полсекунды его застанет.
    let inert_head = ota.image_head();
    let rejected = wait_for(swap_timeout(ota.active_len), || {
        let active_now = try_read_words(chip, &active_hex, HEAD_WORDS)?;
        assert!(
            active_now[..2] != inert_head,
            "неподписанный образ ВСТАЛ в ACTIVE: bootloader пропустил обмен без записи о подписи"
        );
        Ok(ota.state_is(BOOT_MAGIC)? && active_now == original)
    });
    if let Err(why) = rejected {
        panic!(
            "неподписанный образ не отвергнут: запрос на обмен не снят или ACTIVE изменился. \
             Значит, в DFU можно положить чужую прошивку мимо приложения, и она встанет. {why}"
        );
    }
}

/// Ждёт выполнения условия, но не дольше `timeout`.
///
/// Опрос, а не фиксированная пауза: перенос разделов занимает от долей
/// секунды до десятков (зависит от размера страницы и раздела), и любая
/// пауза «на глаз» была бы либо флаки, либо вдвое длиннее нужного.
///
/// Ошибка условия — «ещё не готово», а не провал: пока bootloader стирает
/// флеш, `probe-rs` не может остановить ядро (см. [`try_probe_rs`]). Если
/// время вышло, а последнее чтение отказало, — его текст в `Err`, а не потерян.
fn wait_for(
    timeout: Duration,
    mut done: impl FnMut() -> Result<bool, String>,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        let last = done();
        if let Ok(true) = last {
            // Видно с `--no-capture`: так меряется, сколько оставлено запаса.
            eprintln!("дождались за {:?} из {timeout:?}", start.elapsed());
            return Ok(());
        }
        if start.elapsed() >= timeout {
            return Err(match last {
                Err(why) => format!("Не дождались за {timeout:?}; последнее чтение: {why}"),
                _ => format!("Не дождались за {timeout:?}."),
            });
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
    try_read_words(chip, address, words).unwrap_or_else(|why| panic!("{why}"))
}

/// Чтение, отказ которого `wait_for` переждёт. Не hex в выводе — не временно,
/// паника: разбор разошёлся с форматом `probe-rs`.
fn try_read_words(chip: &str, address: &str, words: usize) -> Result<Vec<u32>, String> {
    let count = words.to_string();
    Ok(
        try_probe_rs(&["read", "--chip", chip, "b32", address, &count])?
            .split_whitespace()
            // probe-rs 0.32 печатает таблицу: адрес строки `08040000:` перед словами.
            .filter(|word| !word.ends_with(':'))
            .map(|word| {
                u32::from_str_radix(word, 16)
                    .unwrap_or_else(|_| panic!("`probe-rs read` вернул не hex-слово: {word:?}"))
            })
            .collect(),
    )
}

/// `0x08020000` из окружения — в число.
fn parse_address(raw: &str) -> u32 {
    let digits = raw.trim_start_matches("0x");
    u32::from_str_radix(digits, 16).unwrap_or_else(|_| panic!("не адрес: {raw}"))
}
