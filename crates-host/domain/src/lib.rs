#![cfg_attr(not(test), no_std)]

// {%- if has_cli == "true" or net_services contains "ssh" %}
/// Консоль платы: общие команды, `shell` и узел `CONSOLE` поверх UART/USB —
/// см. модуль.
pub mod console;
// {%- endif %}

// {%- if graph == "true" %}
/// Узел `HEARTBEAT`: то, что граф из `crates-cross/app` спавнит, а делает —
/// домен. Каркас под работу вашего устройства, см. модуль.
pub mod heartbeat;
// {%- endif %}

// {%- if ota == "true" %}
/// Узел `OTA`: приём образа командами от узла протокола и его применение;
/// приём, применение и решение bootloader'а — подмодули — см. модуль.
pub mod ota;
// {%- endif %}

// {%- if graph == "true" %}
/// Узел `CPU_LOAD`: периодически публикует загрузку процессора; арифметика —
/// подмодуль — см. модуль.
// {%- else %}
/// Загрузка процессора: `log_load` периодически пишет её в лог; арифметика —
/// подмодуль — см. модуль.
// {%- endif %}
pub mod cpu_load;

/// Сетка периодических тактов и deadline для гонок с источниками событий —
/// утилиты циклов узлов, см. модуль.
pub mod tick;

// {%- if has_canopen == "true" %}
/// Узел `CANOPEN_<транспорт>`: узел CANopen на шине CAN — стек `zencan` — см.
/// модуль.
pub mod canopen;
// {%- endif %}
// {%- if settings_port == "true" %}

/// Хранилище настроек поверх `sequential-storage`: пары «ключ — значение»,
/// переживающие перезапуск и обновление прошивки — см. модуль.
pub mod settings;
// {%- endif %}
// {%- if has_protobuf == "true" or net_services != "" or has_cli == "true" %}

/// Сведения об устройстве — для узлов, которые отдают их наружу, см.
/// модуль.
pub mod device;
// {%- endif %}
// {%- if has_protobuf == "true" %}

/// Узел `PROTOBUF_<транспорт>`: сообщения protobuf от хоста — разбор,
/// обработчик приложения, ответ — см. модуль.
pub mod protobuf;
// {%- endif %}
// {%- if has_modbus == "true" %}

/// Modbus: узел протокола `MODBUS_<транспорт>`, slave над картой регистров
/// `modbus::Registers` и, при выборе, master — см. модуль.
// {%- if modbus_master == "true" %}
/// Master — функции поверх `ports::ModbusClient`.
// {%- endif %}
pub mod modbus;
// {%- endif %}

// {%- if has_net == "true" %}
/// Узел `NET`: ведёт сетевой стек `embassy-net` — см. модуль.
pub mod net;
// {%- endif %}

// {%- if net_services contains "rest" %}
/// Узлы `HTTP`: REST API на сокетах сетевого стека — см. модуль.
pub mod http;
// {%- endif %}
// {%- if net_services contains "ssh" %}
/// Узел `SSH`: консоль платы по SSH — см. модуль.
pub mod ssh;
// {%- endif %}
// {%- if net_services contains "mqtt" %}
/// Узел `MQTT`: клиент брокера — см. модуль.
pub mod mqtt;
// {%- endif %}

// {%- if has_usb == "true" %}
/// Узел `USB`: ведёт USB-устройство `embassy-usb` — см. модуль.
pub mod usb;
// {%- endif %}
