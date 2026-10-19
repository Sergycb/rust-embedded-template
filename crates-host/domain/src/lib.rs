#![cfg_attr(not(test), no_std)]

/// Узел `HEARTBEAT`: то, что граф из `crates-cross/app` спавнит, а делает —
/// домен. Каркас под работу вашего устройства, см. модуль.
pub mod heartbeat;

// {%- if ota == "true" %}
/// Узел `OTA`: приём образа командами от узла протокола и его применение;
/// приём, применение и решение bootloader'а — подмодули — см. модуль.
pub mod ota;
// {%- endif %}

/// Узел `CPU_LOAD`: периодически публикует загрузку процессора; арифметика —
/// подмодуль — см. модуль.
pub mod cpu_load;

// {%- if protocol == "canopen" %}
/// Узел `CANOPEN`: узел CANopen на шине CAN — стек `zencan` — см. модуль.
pub mod canopen;
// {%- endif %}
// {%- if protocol == "protobuf" %}

/// Узел `HOST`: сообщения protobuf от хоста по кадровому каналу — разбор,
/// обработчик приложения, ответ — см. модуль.
pub mod host;
// {%- endif %}
// {%- if protocol == "modbus" %}

/// Modbus: slave над портом `ports::Registers` поверх транспорта
/// `ports::ModbusServer` — см. модуль.
// {%- if modbus_master == "true" %}
/// Master — функции поверх `ports::ModbusClient`.
// {%- endif %}
pub mod modbus;
// {%- endif %}

// {%- if interface == "tcp" or interface == "udp" %}
/// Узел `NET`: ведёт сетевой стек `embassy-net` — см. модуль.
pub mod net;
// {%- endif %}

// {%- if interface == "usb" %}
/// Узел `USB`: ведёт USB-устройство `embassy-usb` — см. модуль.
pub mod usb;
// {%- endif %}
