#![doc = include_str!("../../../docs/modules/bsp-wdg.md")]

use core::marker::PhantomData;

use embassy_stm32::rcc::LSI_FREQ;
use embassy_stm32::wdg::{IndependentWatchdog, Instance};
use embassy_time::Duration;
use watchdog::HardwareWatchdog;

/// Аппаратный таймаут сторожа платы — параметр типа [`BoardWatchdog`].
///
/// Само число — в `[env]` crates-cross/.cargo/config.toml
/// (`WATCHDOG_TIMEOUT_US`){% if ota == "true" %}; bootloader берёт его отсюда же, так что разойтись
/// им негде{% endif %}.
///
/// Тринадцать секунд — величина не измеренная, а взятая по цепочке таймаутов
/// (docs/watchdog.md) с поправкой на LSI: это номинал, а реальный таймаут
/// короче — [`HW_TIMEOUT_MIN_US`], у L0/L1 (LSI до 56 кГц при номинале 32)
/// 7.4 с, и он обязан пережить таймаут узла (7 с) плюс `check_every`.
/// Правьте цепочку только вместе. Диапазону IWDG значение удовлетворяет на
/// любом семействе (предел — 26.2 с при LSI 40 кГц, см. `MAX_TIMEOUT_US`
/// ниже).
///
/// **В микросекундах и `u32`, а не `Duration`, потому что это аргумент
/// const-параметра.** Той же цифрой `Iwdg::new` заряжает железо и
/// `HardwareWatchdog::HARDWARE_TIMEOUT` объявляет бюджет — второго
/// источника истины не возникает по построению, и ровно этого требует
/// документация константы в `watchdog`. Побочный выигрыш: половину правого
/// звена цепочки проверяет компилятор — `supervisor_graph!` эмитит
/// `const`-assert «`check_every` строго короче `HARDWARE_TIMEOUT`» (вторую
/// половину, с таймаутом узла, — `const`-assert в `app/src/graph.rs`, где видны
/// оба конца; в эту проверку таймаут узла не входит намеренно, см. `Iwdg::new`),
/// а выход самой цифры за диапазон IWDG становится ошибкой сборки вместо паники
/// на плате.
///
/// Второе требование к нему — пережить (тоже в худшем случае,
/// [`HW_TIMEOUT_MIN_US`]) всё, что стоит между взведением сторожа и первым
/// кормлением от тикера графа: путь {% if ota == "true" %}от прыжка из bootloader'а{% else %}от старта{% endif %} до `spawn_all`;
/// что там окажется в конкретном проекте, шаблон не знает.
{%- if ota == "true" %}
///
/// Той же цифрой взводит сторож release-bootloader (`crates-cross/boot`) —
/// почему цифра одна на оба, docs/watchdog.md.
{%- endif %}
///
/// Уменьшать её имеет смысл вместе с ответом на вопрос «через сколько
/// зависшее устройство обязано вернуться в работу», и не ниже суммы таймаута
/// узла и `check_every` графа — иначе железо сбросит МК раньше, чем
/// программный мультиплексор заметит зависшую задачу, и вся диагностика
/// `supervisor` окажется мёртвым кодом.
pub const HW_TIMEOUT_US: u32 = match u32::from_str_radix(env!("WATCHDOG_TIMEOUT_US"), 10) {
    Ok(us) => us,
    Err(_) => panic!("WATCHDOG_TIMEOUT_US в .cargo/config.toml — не число"),
};

/// Верхний предел частоты LSI у семейства чипа, Гц, — max `f_LSI` из таблицы
/// «LSI oscillator characteristics» даташита; таблица по семействам со
/// ссылками на документы — `watchdog_family_table()` в `chip-select.rhai`
/// репозитория шаблона.
///
/// Нужен затем, что embassy считает прескалер и перезагрузку IWDG от
/// номинала [`LSI_FREQ`], а генератор плавает: у F4 17…47 кГц при номинале
/// 32, у F1 30…60 при 40. Быстрее LSI — раньше сброс.
pub const LSI_MAX_HZ: u32 = {{lsi_max_hz}};

/// Наименьший реальный таймаут сторожа, мкс: [`HW_TIMEOUT_US`] при LSI на
/// верхнем пределе, `HW_TIMEOUT_US * LSI_FREQ / LSI_MAX_HZ`.
///
/// С ним, а не с номиналом, сравнивает таймауты узлов `const`-assert цепочки
/// в `app/src/graph.rs`: на номинале она сходилась бы, а на быстром LSI
/// железо сбрасывало бы МК раньше, чем `supervisor` заметит зависший узел.
/// Медленный LSI (нижний предел) цепочку не рвёт — он лишь дольше ждёт сброса
/// зависшего устройства.
pub const HW_TIMEOUT_MIN_US: u64 = {
    assert!(
        LSI_MAX_HZ >= LSI_FREQ.0,
        "LSI_MAX_HZ ниже номинала LSI_FREQ: в таблице сторожа chip-select.rhai не предел"
    );
    HW_TIMEOUT_US as u64 * LSI_FREQ.0 as u64 / LSI_MAX_HZ as u64
};

/// Нижняя граница: короче этого `IndependentWatchdog` считает окно
/// перезагрузки как `0u16 - 1`. В release (`overflow-checks = false`) это
/// молча даёт максимальное окно вместо минимального — то есть сторож,
/// который якобы стоит на 100 мкс, на деле терпит полсекунды.
const MIN_TIMEOUT_US: u32 = 1_000;

/// Наибольший делитель тактов LSI, который умеет IWDG.
const MAX_PRESCALER: u32 = 256;
/// Наибольшее значение счётчика перезагрузки (`MAX_RL` в `embassy_stm32::wdg`).
const MAX_RELOAD: u32 = 0xFFF;

/// Верхняя граница: дальше не хватает прескалера.
///
/// Считается ровно той же формулой и от той же `LSI_FREQ`, что и внутри
/// embassy (`get_timeout_us(256, MAX_RL)` в `wdg/mod.rs`), и это не
/// педантизм: наша граница обязана быть НЕ ВЫШЕ его предела. Иначе assert
/// ниже пропускает таймаут, на котором embassy сама уходит в панику через
/// `unwrap!`, — а в release паника это сброс, то есть ровно тот цикл
/// перезагрузок, который assert и должен предотвращать.
///
/// Здесь стояло `30_000_000` «с запасом вниз от 32 кГц», и рассуждение было
/// перевёрнуто: частота LSI по семействам разная (40 кГц у F0/F1/F3, 32 кГц у
/// остальных — `rcc/bd.rs`), но выше частота — КОРОЧЕ максимальное окно. На
/// 40 кГц предел 26.2 с, то есть таймаут 28 с проходил проверку и падал
/// внутри embassy.
const MAX_TIMEOUT_US: u32 = 1_000_000 * (MAX_RELOAD + 1) / (LSI_FREQ.0 / MAX_PRESCALER);

/// Сторож этой платы, уже взведённый: блок IWDG выбранного чипа и таймаут
/// [`HW_TIMEOUT_US`] — то, что граф задач называет в блоке `watchdog:`.
///
/// Псевдоним, а не голый [`Iwdg`] в графе, по общему правилу шаблона: тип,
/// уезжающий в `#[embassy_executor::task]`, приходит из `bsp` под именем —
/// иначе имя блока (`IWDG`, `IWDG1`, `IWDG2` — зависит от чипа) пришлось бы
/// писать в `app`, где про чип не знают.
pub type BoardWatchdog = Iwdg<embassy_stm32::peripherals::{{watchdog_peripheral}}, HW_TIMEOUT_US, Armed>;

/// Он же до взвода — то, что лежит полем [`crate::Board`] и что
/// [`Iwdg::arm`] превращает в [`BoardWatchdog`].
///
/// Разными типами, а не флагом внутри одного, потому что разница здесь
/// смысловая: [`HardwareWatchdog`] реализован только у взведённого, значит
/// невзведённый графу не отдать — «сторож есть, а система не защищена»
/// перестаёт быть достижимым состоянием.
pub type UnarmedBoardWatchdog = Iwdg<embassy_stm32::peripherals::{{watchdog_peripheral}}, HW_TIMEOUT_US, Disarmed>;

impl UnarmedBoardWatchdog {
    /// Заряжает сторожевой таймер, не запуская его.
    ///
    /// Регистры прескалера и перезагрузки заполняются здесь, счётчик стоит:
    /// IWDG начинает отсчёт только с [`arm`](Self::arm). Поэтому конструктор
    /// зовётся там, где создаются все остальные объекты платы
    /// (`Board::new`), и ничего этим не запускает.
    ///
    /// # Отказ на компиляции
    ///
    /// Если `HW_TIMEOUT_US` вне диапазона, который IWDG умеет отсчитать.
    /// Проверка стоит здесь, а не оставлена embassy, по разным причинам с
    /// двух краёв: сверху `IndependentWatchdog::new` паникует сам (а на
    /// release паника — это сброс, то есть цикл перезагрузок из-за опечатки в
    /// конфигурации), снизу же он молча вычисляет `0u16 - 1` и ставит
    /// МАКСИМАЛЬНОЕ окно вместо запрошенного минимального — с
    /// `overflow-checks = false` в release это не заметит никто.
    ///
    /// Рантайм-`assert!` был бы той самой паникой, от которой он защищает,
    /// только со своим текстом. Поэтому таймаут — константа, и та же
    /// проверка отвечает `error[E0080]` при сборке.
    pub(crate) fn new(resources: crate::resources::WatchdogResources) -> Self {
        // Сообщение без цифр: `assert!` в const-контексте берёт только
        // литерал, форматировать ему нечем. Цифры вызывающий видит в спане
        // ошибки — там стоит его собственный `HW_TIMEOUT_US`.
        const {
            assert!(
                HW_TIMEOUT_US >= MIN_TIMEOUT_US && HW_TIMEOUT_US <= MAX_TIMEOUT_US,
                "таймаут IWDG вне диапазона, который умеет отсчитать блок: см. MIN_TIMEOUT_US и \
                 MAX_TIMEOUT_US в bsp/src/wdg.rs",
            );
        }
        Self {
            inner: IndependentWatchdog::new(resources.iwdg, HW_TIMEOUT_US),
            state: PhantomData,
        }
    }
}

/// Сторож заряжен, но не запущен: регистры заполнены, счётчик стоит.
#[derive(Debug)]
pub struct Disarmed;

/// Сторож тикает. Только в этом состоянии он реализует [`HardwareWatchdog`].
#[derive(Debug)]
pub struct Armed;

pub struct Iwdg<T: Instance + 'static, const TIMEOUT_US: u32, S> {
    inner: IndependentWatchdog<'static, T>,
    state: PhantomData<S>,
}

impl<T: Instance + 'static, const TIMEOUT_US: u32, S> core::fmt::Debug for Iwdg<T, TIMEOUT_US, S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Iwdg")
            .field("timeout_us", &TIMEOUT_US)
            .finish_non_exhaustive()
    }
}

impl<T: Instance + 'static, const TIMEOUT_US: u32> Iwdg<T, TIMEOUT_US, Disarmed> {
    /// Запускает сторож. Выключить его после этого нельзя: IWDG
    /// останавливается только сбросом.
    ///
    /// **Отсчёт начинается здесь, а кормить его начнёт только граф задач** —
    /// не раньше первого тика `check_every`. Поэтому вызов — инициализатор
    /// блока `watchdog:` (`app/src/graph.rs`), который исполняет пролог
    /// `spawn_all`, а не `Board::new` и не `main` (docs/watchdog.md).
{%- if ota == "true" %}
    ///
    /// Исключение — release-сборка с bootloader'ом: там IWDG уже запущен им
    /// (docs/watchdog.md), вызов лишь подтверждает это, а таймаут, заданный
    /// [`Iwdg::new`], в силу вступает со следующего кормления.
{%- endif %}
    ///
    /// Забыть этот вызов нельзя: [`HardwareWatchdog`] реализован только у
    /// [`Armed`], а блок `watchdog:` графа требует именно его.
    ///
    /// В dev-сборке (`debug_assertions`) он ещё и замораживает сторож на время
    /// остановки ядра отладчиком — `freeze_on_halt`.
    ///
    /// Аппаратный таймаут должен быть **больше** таймаута узла в графе плюс
    /// `check_every` (цепочка — docs/watchdog.md). Половину этого соотношения —
    /// `check_every` против аппаратного таймаута —
    /// с объявлением [`HardwareWatchdog::HARDWARE_TIMEOUT`] проверяет
    /// компилятор; таймаут узла в эту проверку не входит намеренно, его
    /// проверяет `const`-assert в `app/src/graph.rs` — там, где видны оба
    /// конца, см. её документацию в `watchdog`.
    #[must_use]
    pub fn arm(self) -> Iwdg<T, TIMEOUT_US, Armed> {
        #[cfg(debug_assertions)]
        freeze_on_halt();
        let mut inner = self.inner;
        inner.unleash();
        Iwdg {
            inner,
            state: PhantomData,
        }
    }
}

impl<T: Instance + 'static, const TIMEOUT_US: u32> HardwareWatchdog for Iwdg<T, TIMEOUT_US, Armed> {
    /// Тот же `TIMEOUT_US`, которым [`UnarmedBoardWatchdog::new`] заряжает железо, — потому и
    /// объявлен: константа связывает ровно настолько, насколько реализация
    /// заряжает железо той же цифрой, а здесь цифра одна на оба места по
    /// построению (см. `HW_TIMEOUT_US`).
    ///
    /// `from_micros_floor`, а не `from_micros`: `Duration` embassy считает в
    /// тиках, и округление вверх объявляло бы бюджет чуть больше настоящего —
    /// то есть в пользу проверяемого, а не проверки.
    const HARDWARE_TIMEOUT: Option<Duration> = Some(Duration::from_micros_floor(TIMEOUT_US as u64));

    fn feed(&mut self) {
        self.inner.pet();
    }

    /// Последняя строка перед сбросом — какой узел перестал отмечаться.
    /// `supervisor` сам ничего не печатает, и без неё сброс от сторожа в логе
    /// ничем не отличался бы от любого другого. Только `defmt` — зовётся под
    /// блокировками проверки, ждать здесь нельзя.
    fn before_reset(&mut self, overdue: &'static str) {
        defmt::error!(
            "wdg: узел {} не отметился вовремя — аппаратный сброс",
            overdue
        );
    }

    fn trigger_reset(&mut self) -> ! {
        // Просто перестаём кормить и ждём. Не `SCB::sys_reset()`, хотя тот
        // сработал бы быстрее: сброс должен прийти именно от сторожевого
        // таймера, иначе в регистре флагов сброса он неотличим от
        // программного перезапуска — а разница ровно та, ради которой
        // watchdog и ставят («устройство зависло» против «его попросили
        // перезапуститься»).
        loop {
            cortex_m::asm::wfi();
        }
    }
}

/// Останавливает счётчик IWDG, пока ядро остановлено отладчиком (бит
/// заморозки в `DBGMCU`).
///
/// Без него IWDG тикает и под брейкпоинтом: остановка дольше таймаута
/// сбрасывает МК, и отладка dev-сборки превращается в гонку со сторожем.
/// Только в dev-сборке: в release сторож обязан сбрасывать и зависшее под
/// отладчиком устройство — так он ведёт себя в поле. Ставится до запуска
/// счётчика, чтобы первая же остановка была под заморозкой. Регистр и поле —
/// из `stm32-metapac` по семейству (`watchdog_family_table()` в
/// `chip-select.rhai` шаблона).
{%- if dbgmcu_freeze == "" %}
///
/// У этого блока бита заморозки нет в `stm32-metapac` (`IWDG2` ядра CM4
/// двухъядерных H7), и функция пуста: сторож тикает и под отладчиком
/// (docs/watchdog.md).
{%- endif %}
///
/// Бит живёт до сброса по питанию, а не до системного (на H7 проверено): прошитая следом
/// release-сборка на той же плате тоже будет заморожена под отладчиком.
#[cfg(debug_assertions)]
{%- if dbgmcu_freeze == "" %}
fn freeze_on_halt() {}
{%- else %}
fn freeze_on_halt() {
{%- assign fz = dbgmcu_freeze | split: ":" %}
{%- if dbgmcu_clock != "" %}
{%- assign ck = dbgmcu_clock | split: ":" %}
    use embassy_stm32::pac::{DBGMCU, RCC};

    // Без такта блока запись в `DBGMCU` теряется.
    RCC.{{ ck[0] }}().modify(|w| w.set_{{ ck[1] }}(true));
{%- else %}
    use embassy_stm32::pac::DBGMCU;

{%- endif %}
    DBGMCU.{{ fz[0] }}().modify(|w| w.set_{{ fz[1] }}(true));
}
{%- endif %}
