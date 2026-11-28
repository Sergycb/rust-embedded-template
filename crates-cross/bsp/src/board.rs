//! Объекты платы: `Board` собирает их из чипа и отдаёт приложению.
//!
//! Границу `bsp` → `app` пересекают только они — рычаги платы
//! (docs/architecture.md, «`Board` отдаёт рычаги платы, а не периферию и не
//! протоколы»).


{%- assign xs = links | split: "," %}

/// Объекты этой платы — всё, что приложение о ней знает.
///
/// Каждое поле — рычаг платы под трейтом или данные о чипе; `Peripherals`,
/// выводы и группы распиновки остаются в чипе — [`Board::new`] их не видит.{% if has_usb == "true" or has_net == "true" %} {% if has_usb == "true" %}Пары USB-устройства с потоком CDC (классы
/// регистрируются до постройки устройства){% if has_net == "true" %} и сетевого стека с фоновой работой
/// (один `embassy_net::new`){% endif %} собраны здесь же — иначе их не разделить{% else %}Пара сетевого стека с фоновой
/// работой (один `embassy_net::new`) собрана здесь же — иначе её не разделить{% endif %}.{% endif %}
{%- if has_usb == "true" or has_net == "true" %}
pub struct Board<C: ports::Cpu>
where
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}
    C::{% for part in parts %}{{ part | capitalize }}{% endfor %}: embassy_usb::driver::Driver<'static>,
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}
    C::Ethernet: embassy_net::driver::Driver + 'static,
{%- endif %}
{
{%- else %}
pub struct Board<C: ports::Cpu> {
{%- endif %}
    /// Сведения о чипе: имя, заводской номер, объём flash, частота `sys`.
    pub cpu_info: C::CpuInfo,
    /// Сырой флеш чипа (`cpu::flash::Flash`) — `Copy`-ручка: из неё
    /// compose-site строит разделы обновления и носитель настроек, кому они
    /// нужны.
    pub flash: C::Flash,
{%- if external_flash == "true" %}
    /// Внешняя флешь W25Q (`cpu::external::ExtFlash`) — `Copy`-ручка: разделы на
    /// ней строит compose-site. Микросхему опознаёт `cpu::external::init`,
    /// когда `app` знает, сколько на ней лежит.
    pub ext_flash_bus: C::ExtFlashBus,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- assign parts = f[4] | split: "_" %}
{%- if f[0] == "uart" %}
    /// UART `{{ f[2] }}` — поток байтов; драйвер — `cpu::com::{{ f[4] }}::Serial`, до
    /// подключения выводов заглушка (`docs/modules/cpu-com.md`). {% if f[3] == "none" %}Протокола у транспорта
    /// нет: поле ждёт ваш узел.{% else %}Канал протокола {{ f[3] }} над ним строит
    /// `app` — {% if graph == "true" %}в привязке узла в `crates-cross/app/src/graph.rs`{% else %}в задаче `crates-cross/app/src/tasks/{{ f[3] }}.rs`{% endif %}.{% endif %}
    pub {{ f[4] }}: C::{% for part in parts %}{{ part | capitalize }}{% endfor %},
{%- elsif f[0] == "can" %}
    /// Шина CAN `{{ f[2] }}`{% if f[3] == "canopen" %} под узлом `CANOPEN_{{ f[4] | upcase }}`{% endif %} — драйвер `cpu::can::{{ f[4] }}::Can`, пока проект не
    /// подставит контроллер своей платы (`docs/modules/cpu-can.md`).
{%- if f[3] == "canopen" %}
{%- if graph == "true" %}
    /// Уезжает в узел `CANOPEN_{{ f[4] | upcase }}`: в граф (`fragments:` в `crates-cross/app/src/graph.rs`).
{%- else %}
    /// Уезжает в узел `CANOPEN_{{ f[4] | upcase }}`: в модуль задачи (`crates-cross/app/src/tasks/canopen.rs`).
{%- endif %}
{%- else %}
    /// Протокола у транспорта нет: поле ждёт ваш узел.
{%- endif %}
    pub {{ f[4] }}: C::{% for part in parts %}{{ part | capitalize }}{% endfor %},
{%- else %}
    /// USB `{{ f[2] }}`: устройство `embassy-usb` (`device`, узел `USB_{{ f[4] | upcase }}`) и
    /// поток CDC (`cdc`) — {% if f[3] == "none" %}протокола у транспорта нет: поток ждёт ваш узел{% else %}под транспортом протокола {{ f[3] }}{% endif %}. Драйвер — заглушка
    /// чипа, пока проект не подставит USB-блок своей платы (`docs/modules/bsp-usb.md`).
    pub {{ f[4] }}: crate::usb::{{ f[4] }}::Usb<C::{% for part in parts %}{{ part | capitalize }}{% endfor %}>,
{%- endif %}
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
    /// Сеть: фоновая работа стека `embassy-net` (`runner`, узел `NET`) и стек
    /// (`stack`) — для сокетов транспортов и служб приложения. Устройство —
    /// заглушка чипа, пока проект не подставит Ethernet (`docs/modules/bsp-ethernet.md`).
    pub ethernet: crate::ethernet::Net<C::Ethernet>,
{%- endif %}
    /// Система чипа: перезагрузка (`ports::Reboot`) и причина прошлого сброса.
    pub system: C::System,
{%- if graph == "true" %}
    /// Сторож платы — заряженный, но ещё не запущенный: его забирает
    /// инициализатор блока `watchdog:` в графе задач
    /// (`board.watchdog.arm()`).
    ///
    /// Запускается там, а не здесь, потому что с этого момента железо тикает,
    /// а кормит его только тикер графа: чем короче промежуток между двумя
    /// этими точками, тем меньше шансов уйти в перезагрузки молча. Короче
    /// всего он в прологе `spawn_all`. Забыть запуск нельзя — блок
    /// `watchdog:` требует тип, реализующий `HardwareWatchdog`, а он есть
    /// только у запущенного (`cpu::wdg`).
    pub watchdog: C::Watchdog,
{%- endif %}
}

// Вручную: у драйверов чипа `Debug` есть не у всех, а печатать целую плату
// незачем — сведения о чипе лежат в `cpu_info`.
{%- if has_usb == "true" or has_net == "true" %}
impl<C: ports::Cpu> core::fmt::Debug for Board<C>
where
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}
    C::{% for part in parts %}{{ part | capitalize }}{% endfor %}: embassy_usb::driver::Driver<'static>,
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}
    C::Ethernet: embassy_net::driver::Driver + 'static,
{%- endif %}
{
{%- else %}
impl<C: ports::Cpu> core::fmt::Debug for Board<C> {
{%- endif %}
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Board")
            .field("cpu_info", &self.cpu_info)
            .finish_non_exhaustive()
    }
}

{%- if has_usb == "true" or has_net == "true" %}
impl<C: ports::Cpu> Board<C>
where
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}
    C::{% for part in parts %}{{ part | capitalize }}{% endfor %}: embassy_usb::driver::Driver<'static>,
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}
    C::Ethernet: embassy_net::driver::Driver + 'static,
{%- endif %}
{
{%- else %}
impl<C: ports::Cpu> Board<C> {
{%- endif %}
    /// Раскладывает готовый чип по полям платы и собирает интерфейсы над его
    /// драйверами.
    ///
    /// HAL поднят раньше — в `cpu::init`; здесь ничего не запускается: ни
    /// сторож, ни периферия не настраивается «на будущее». Внешняя флешь тоже
    /// не опознаётся — это делает `cpu::external::init`, когда `app` знает,
    /// сколько на ней лежит.
    ///
    /// Зовётся ровно один раз за запуск: чип потребляется, второго [`Board`] из
    /// него не собрать.
    #[allow(
        clippy::similar_names,
        reason = "имена транспортов — имена блоков чипа: usb_otg_fs и usb_otg_hs различаются одной буквой"
    )]
    #[must_use]
    pub fn new(cpu: C) -> Self {
        // Номер чипа нужен до `split`: серийные номера USB-устройств.
        let {% if has_usb == "true" %}uid{% else %}_uid{% endif %} = cpu.uid();
        let (
            // По рычагу на строку: их набор зависит от генерации.
            cpu_info,
            flash,
{%- if external_flash == "true" %}
            ext_flash_bus,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
            {{ f[4] }},
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
            ethernet,
{%- endif %}
            system,
{%- if graph == "true" %}
            watchdog,
{%- endif %}
{%- if has_net == "true" %}
            seed,
{%- endif %}
        ) = cpu.split();

        Self {
            cpu_info,
            flash,
{%- if external_flash == "true" %}
            ext_flash_bus,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- if f[0] == "usb" %}
            {{ f[4] }}: crate::usb::{{ f[4] }}::new({{ f[4] }}, uid),
{%- else %}
            {{ f[4] }},
{%- endif %}
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
            ethernet: crate::ethernet::new(ethernet, seed),
{%- endif %}
            system,
{%- if graph == "true" %}
            watchdog,
{%- endif %}
        }
    }
}
