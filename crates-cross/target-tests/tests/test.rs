//! Тесты, исполняемые на самом устройстве (`cargo xtask test target`).
//!
//! Средний этап трёхэтапной модели: host-тесты (`cargo xtask test host`)
//! гоняют логику `domain` на хосте и про железо ничего не знают, а
//! host-target тесты смотрят на готовое устройство снаружи, через его протокол.
//! Здесь — код,
//! которому нужен настоящий чип: тактирование, драйвер времени, периферия,
//! ровно та инициализация, которую делает `Board`.
//!
//! Как это работает: `probe-rs run` (он же runner в `crates-cross/.cargo/config.toml`)
//! вычитывает список тестов прямо из ELF, заливает его один раз, а дальше на
//! каждый тест сбрасывает МК и говорит прошивке через semihosting, какой
//! именно тест выполнять — поэтому у каждого теста своя, чистая инициализация,
//! и `#[init]` ниже вызывается для каждого из них заново (иначе повторный
//! `embassy_stm32::init` паниковал бы).
//!
//! Тесты ниже — минимальный каркас: они проверяют то, что верно для любой
//! платы, и потому переживают генерацию под любой чип. Настоящие проверки
//! драйверов (UART отвечает, датчик отдаёт правдоподобные значения) дописывать
//! сюда же, по мере того как `Board` начнёт отдавать реальную периферию.

#![no_std]
#![no_main]

// Транспорт тот же, что в `app` (см. crates-cross/app/src/run.rs), а паникёр —
// нет: здесь его даёт сам `embedded-test`, и это принципиально. Его
// `#[panic_handler]` заканчивается `semihosting::process::abort()`, который
// `probe-rs run` читает как «этот тест упал» и переходит к следующему;
// `panic-probe` вместо этого делает `udf()`, то есть HardFault, и один
// провалившийся `assert!` унёс бы с собой весь прогон.
use defmt_rtt as _;

#[embedded_test::tests]
mod tests {
    use bsp::Board;
    use embassy_time::{Duration, Instant, Timer};

    /// Частота, которую вы задумали для этой платы, в герцах. `None` —
    /// проверка выключена, тест только печатает фактическое тактирование
    /// (дамп делает `Board::new`) и следит за инвариантами ниже.
    ///
    /// Шаблон не может подставить сюда число: он знает чип, но не знает вашу
    /// плату — есть ли кварц, на какую частоту вы её считали. Заполняется один
    /// раз, когда тактирование настроено (`embassy_stm32::Config` в
    /// `config()` в `bsp/src/board.rs`): после этого любая правка PLL, которая
    /// изменила результат, роняет тест вместо того, чтобы всплыть через
    /// неделю кривым битрейтом.
    const EXPECTED_SYS_HZ: Option<u32> = None;
{%- if ota == "true" %}

    /// Чем читается стёртая страница флеша этого чипа.
    ///
    /// Подставляется при генерации из того же признака, по которому у
    /// `embassy-boot` включается фича `flash-erase-zero`: у L0/L1 стёртое
    /// состояние нулевое, у остальных семейств — `0xFF`.{% if ota_store == "external" %} С разделами на
    /// внешней флеши — `0xFF` всегда: NOR стирается в единицы.{% endif %}
    const ERASED: u8 = {% if erase_zero == "true" and ota_store != "external" %}0x00{% else %}0xFF{% endif %};

    /// Заведомо НЕ стёртые байты — инверсия стёртого состояния.
    ///
    /// Именно инверсия, а не литерал: тесты пачкают ими раздел, чтобы отличить
    /// работающее стирание от заглушки, и `0x00` на чипе со стиранием в ноль
    /// мусором не является. Длина кратна `WRITE_SIZE` самых крупных чипов (у
    /// H7 это 32 байта) — короче раздел записи не принимает.
    const DIRTY: [u8; 32] = [!ERASED; 32];
{%- endif %}

{%- if ota == "true" %}
    /// Адаптер обновления над разделами платы — так же, как его строит
    /// compose-site узла OTA.
    fn updater(firmware: bsp::FirmwareFlash) -> domain::ota::Updater<bsp::FirmwareFlash> {
        domain::ota::Updater::new(firmware)
    }
{%- endif %}
{%- if config == "true" and config_store != "external" %}
    /// Хранилище настроек на весь носитель платы — так же, как его строит
    /// compose-site узла-потребителя.
    fn settings(flash: bsp::ConfigFlash) -> domain::settings::Settings<bsp::ConfigFlash> {
        domain::settings::Settings::whole(flash)
    }
{%- endif %}

    /// Выполняется перед каждым тестом (устройство между ними сброшено) и
    /// отдаёт результат тесту параметром.
    #[init]
{%- if external_flash == "true" %}
    async fn init() -> Board {
        Board::new().await
    }
{%- else %}
    fn init() -> Board {
        Board::new()
    }
{%- endif %}

    /// Драйвер времени реально тикает. Проверка дешёвая, но ловит целый класс
    /// молчаливых поломок конфигурации (не тот источник тактирования, не
    /// включённый таймер): прошивка при этом стартует и «работает», а любой
    /// `Timer::after` висит вечно.
    #[test]
    async fn time_driver_advances(_board: Board) {
        let start = Instant::now();
        Timer::after(Duration::from_millis(10)).await;
        assert!(Instant::now() - start >= Duration::from_millis(10));
    }

    /// Тактирование получилось таким, каким задумано.
    ///
    /// С заполненной `EXPECTED_SYS_HZ` — единственная проверка в проекте,
    /// которая ловит расхождение «заказали одно — получили другое»:
    /// компилятор про частоты не знает ничего, а embassy проверяет только
    /// верхние границы шин (`assert!(hclk <= hclk_max)`). Конфигурация,
    /// дающая USB 47.5 вместо 48 мегагерц или UART с ошибкой по битрейту,
    /// стартует молча. С `None` тест только печатает дамп и предупреждает,
    /// что проверка выключена.
    ///
    /// Проверяется только `sys` — единственный домен, который заполняют все
    /// семейные реализации `rcc` в embassy; остальной состав `Clocks`
    /// генерируется под чип и в шаблоне неизвестен.
    #[test]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "`embedded-test` отдаёт состояние `#[init]` по значению"
    )]
    fn clocks_match_intent(board: Board) {
        // Частота приходит данными из `Board::spec`: `Board::new` считал её у
        // HAL сам, пока владел `RCC`.
        let sys = board
            .spec
            .sys_hz
            .expect("частота ядра должна быть посчитана");

        if let Some(expected) = EXPECTED_SYS_HZ {
            assert_eq!(
                sys, expected,
                "тактирование разошлось с задуманным (EXPECTED_SYS_HZ)"
            );
        } else {
            // Пока константа не заполнена, тест по сути только печатает дамп,
            // и зелёная галочка без этого предупреждения означала бы
            // «проверено», хотя не проверено ничего.
            defmt::warn!(
                "clocks_match_intent: EXPECTED_SYS_HZ не заполнена — проверка выключена, sys = {} Гц",
                sys,
            );
        }
    }
{%- if ota == "true" %}
{%- if ota_store == "external" %}

    /// С разделами на внешней флеши тесты ниже проверяют подставленный драйвер
    /// микросхемы. Пока на его месте заглушка (`bsp/src/external.rs`),
    /// проверять нечего: тест пропускается с предупреждением, а не падает.
    fn no_external_flash() -> bool {
        let stub = !bsp::external::connected();
        if stub {
            defmt::warn!("внешняя флеш не подключена (bsp/src/external.rs) — тест пропущен");
        }
        stub
    }
{%- endif %}

    /// `prepare(len)` стирает раздел так, как ждёт `embassy-boot`, и целиком —
    /// всё, что обмен перенесёт в `ACTIVE`, а не только страницы под образ.
    ///
    /// Первое: стёртый флеш читается тем же значением, которое считает стёртым
    /// `embassy-boot`. Проверка выглядит тавтологией ровно до того момента, как
    /// попадётся чип, где это не так. У STM32L0 и L1 стёртая страница читается
    /// **нулями**, а `embassy-boot` по умолчанию ждёт `0xFF` и переключается
    /// фичей `flash-erase-zero`. Не включи её — обмен разделов читает признак
    /// «стёрто» наоборот, и OTA не работает, ничего при этом не сообщая.
    /// Ожидаемое значение подставляется при генерации из того же признака, по
    /// которому включается фича, так что тест сверяет две стороны одного
    /// решения: что мы сказали `embassy-boot` и что на самом деле делает чип.
    ///
    /// Второе: короткий образ, а стёрты и последняя страница `ACTIVE`, и
    /// запасная за ней. Подпись покрывает только образ, а обмен копирует
    /// раздел целиком: нестёртый хвост с остатками прошлых загрузок уехал бы в
    /// `ACTIVE` непроверенным (docs/ota.md, «Приём образа»). Все прочие вызовы
    /// `prepare` в тестах укладываются в одну страницу, и «стираем всё» там не
    /// отличить от «стираем под образ».
    #[test]
    async fn prepare_erases_what_the_swap_moves(board: Board) {
{%- if ota_store == "external" %}
        if no_external_flash() {
            return;
        }

{%- endif %}
        let mut ota = updater(board.firmware);
        let page = ota.erase_size();
        let capacity = ota.capacity();
        // Последняя страница `ACTIVE`: дальше всего от короткого образа.
        let tail = capacity - page;

        // Пачкаем раздел перед стиранием: иначе тест зелёный и в случае, когда
        // `prepare` не сделал ничего, а байты остались стёртыми с прошлого
        // прогона. Мусор — инверсия ожидаемого, чтобы он был мусором и там,
        // где стёртое состояние нулевое. Запасная страница (`capacity`) —
        // туда ляжет запись для bootloader'а.
        ota.prepare(capacity)
            .await
            .expect("раздел DFU должен стираться");
        ota.write(0, &DIRTY).await.expect("запись мусора в начало");
        if tail != 0 {
            ota.write(tail, &DIRTY)
                .await
                .expect("запись мусора в последнюю страницу ACTIVE");
        }
        ota.write(capacity, &DIRTY)
            .await
            .expect("запись мусора в запасную страницу");
        // Прочитанный мусор оседает в кэше данных (D-cache M7, `ICACHE`
        // L5/U5/H5, ART F2/F4): стирание ниже обязано его не оставить, иначе
        // голова раздела прочитается мусором, а не стёртой.
        let mut dirty = [ERASED; 8];
        ota.read(0, &mut dirty).await.expect("чтение мусора");
        assert_eq!(dirty, DIRTY[..8], "мусор не записался");

        ota.prepare(8).await.expect("раздел DFU должен стираться");

        let mut head = [!ERASED; 8];
        ota.read(0, &mut head)
            .await
            .expect("раздел DFU должен читаться");
        assert_eq!(
            head, [ERASED; 8],
            "стёртый флеш читается не тем значением, которое ждёт embassy-boot: \
             проверьте фичу flash-erase-zero и признак erase_zero в chip-select.rhai"
        );

        let mut read = [!ERASED; 32];
        ota.read(tail, &mut read)
            .await
            .expect("чтение последней страницы ACTIVE");
        assert_eq!(
            read, [ERASED; 32],
            "хвост DFU за образом не стёрт — обмен перенёс бы его в ACTIVE непроверенным"
        );
        ota.read(capacity, &mut read)
            .await
            .expect("чтение запасной страницы");
        assert_eq!(
            read, [ERASED; 32],
            "запасная страница не стёрта — запись для bootloader'а легла бы на мусор"
        );
    }

    /// Два куска в ОДИН сектор — то, из чего состоит любой реальный приём
    /// образа по частям.
    ///
    /// Тест не теоретический: до появления `prepare` каждая запись стирала
    /// сектор, в который писала (`write_firmware` из `embassy-boot` помнит
    /// стёртый сектор в объекте апдейтера, а тот создавался заново на каждый
    /// вызов). Первый кусок читался как стёртый — проверено на живой плате.
    /// Проверять это на хосте нечем: подделка флеша ведёт себя не как
    /// настоящий, а вся суть именно в стирании.
    #[test]
    async fn dfu_partition_keeps_both_writes_into_one_sector(board: Board) {
{%- if ota_store == "external" %}
        if no_external_flash() {
            return;
        }

{%- endif %}
        let mut ota = updater(board.firmware);

        let first = [0xA1u8; 32];
        let second = [0xB2u8; 32];

        // Сначала пачкаем раздел заведомо непустыми байтами: без этого тест не
        // отличил бы работающий `prepare` от `Ok(())`, ведь раздел мог
        // остаться стёртым с прошлого прогона.
        ota.prepare(32).await.expect("раздел DFU должен стираться");
        ota.write(0, &DIRTY).await.expect("запись мусора");

        ota.prepare(96).await.expect("раздел DFU должен стираться");
        let mut read = [!ERASED; 32];
        ota.read(0, &mut read).await.expect("чтение после стирания");
        assert_ne!(
            read, DIRTY,
            "prepare не стёр раздел: записанное осталось на месте"
        );

        ota.write(0, &first).await.expect("первая запись");
        ota.write(64, &second).await.expect("вторая запись");

        ota.read(0, &mut read).await.expect("чтение первого куска");
        assert_eq!(
            read, first,
            "первый кусок затёрт вторым — приём образа по частям не работает"
        );
        ota.read(64, &mut read).await.expect("чтение второго куска");
        assert_eq!(read, second);
    }
{%- endif %}
{%- if ota == "true" %}

    /// Образ не новее текущего отвергается — защита от атаки отката.
    ///
    /// Смысл защиты: подпись доказывает, КТО прислал образ, но не то, что он
    /// новее. Версия едет в последних четырёх байтах образа (их дописывает
    /// `cargo xtask build`), поэтому тест их туда и кладёт: подделать их
    /// злоумышленник не может, они внутри подписанных байтов, а вот тесту
    /// достаточно записи в `DFU`. Перед версией — отпечаток раскладки самой
    /// платы (`Updater::layout`): без него отказ пришёл бы раньше
    /// версии, `Layout`, и `Rollback` тест бы не увидел.
    #[test]
    async fn ota_rejects_an_older_image(board: Board) {
        use domain::ota::update::UpdateError;

        const LENGTH: u32 = 32;
        const LONGER: u32 = LENGTH * 2;

        // Версия «этой прошивки» в тесте — 0.1.0: ниже `u32::MAX` и не ниже
        // `0.0.0`, которые кладутся в хвост образа.
        const RUNNING: u32 = domain::ota::version::pack(0, 1, 0);

{%- if ota_store == "external" %}
        if no_external_flash() {
            return;
        }

{%- endif %}
        let mut ota = updater(board.firmware);

        // Кратно WRITE_SIZE флеша. Хвост — раскладка этой прошивки (иначе
        // отказ `Layout` раньше версии) и версия `0.0.0`, заведомо не новее
        // любой проставленной в проекте.
        let layout = ota.layout().to_le_bytes();
        let mut image = [0xFFu8; LENGTH as usize];
        image[LENGTH as usize - 8..LENGTH as usize - 4].copy_from_slice(&layout);
        image[LENGTH as usize - 4..].copy_from_slice(&0u32.to_le_bytes());
        ota.prepare(LENGTH * 2)
            .await
            .expect("раздел DFU должен стираться");
        ota.write(0, &image)
            .await
            .expect("раздел DFU должен принимать запись");

        let refused = domain::ota::update::apply(&mut ota, RUNNING, &[0u8; 64], LENGTH).await;
        assert!(
            matches!(refused, Err(UpdateError::Rollback { .. })),
            "старый образ должен отвергаться отдельной ошибкой, а не общей"
        );

        // Обратная сторона: образ с версией заведомо новее проверку версии
        // ПРОХОДИТ — и доходит до записи для bootloader'а с пометкой обмена.
        // Подпись тут нули, и bootloader на следующем сбросе отверг бы образ,
        // но тест снимает пометку сам (`mark_booted`), чтобы не оставлять
        // запрос на обмен соседям по прогону. Второй образ кладётся
        // РЯДОМ, а не поверх первого: хвост читается по `length - 8`, так что
        // достаточно попросить проверить кусок подлиннее, а перезаписать те же
        // восемь байт нельзя (`0xFF` поверх `0x00` не ложится).
        let mut newer = [0xFFu8; LONGER as usize];
        newer[LONGER as usize - 8..LONGER as usize - 4].copy_from_slice(&layout);
        newer[LONGER as usize - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        ota.write(LENGTH, &newer[LENGTH as usize..])
            .await
            .expect("раздел DFU должен принимать запись");

        let accepted = domain::ota::update::apply(&mut ota, RUNNING, &[0u8; 64], LONGER).await;
        ota.mark_booted().await.expect("снять запрос на обмен");
        assert!(
            accepted.is_ok(),
            "образ новее текущего должен дойти до записи для bootloader'а: {accepted:?}"
        );
    }
{%- endif %}
{%- if config == "true" and config_store != "external" %}

    /// Раздел настроек принимает запись и отдаёт её обратно.
    ///
    /// {% if ota == "true" %}Проверка того же рода, что и для `DFU`, и по той же причине: границы{% else %}Границы{% endif %}
    /// раздела приходят из символов линкера, размер страницы — из метаданных
    /// чипа, а требования `sequential-storage` (две страницы, выравнивание по
    /// их границам) проверяются только в рантайме. Разъехаться они могут
    /// исключительно на живом чипе.
    ///
    /// Тест пишет в хранилище при каждом прогоне — это нормально: запись идёт
    /// дозаписью в конец страницы, стирание случается, только когда страница
    /// кончилась.
    #[test]
    async fn settings_store_and_read_back(board: Board) {
        const KEY: u32 = 0;
        let written = b"template";
        let mut scratch = [0u8; 32];
        let mut settings = settings(board.config);

        settings
            .write(KEY, written)
            .await
            .expect("хранилище настроек должно принимать запись");
        let read = settings
            .read(KEY, &mut scratch)
            .await
            .expect("хранилище настроек должно читаться");

        assert_eq!(read, Some(&written[..]));
    }
{%- endif %}
}
