#![cfg_attr(not(test), no_std)]

/// Узел `HEARTBEAT`: то, что граф из `crates-cross/app` спавнит, а делает —
/// домен. Каркас под работу вашего устройства, см. модуль.
pub mod heartbeat;

/// Узел `OTA`: приём образа по каналу и его применение — в цикле, пока жив
/// канал; приём и применение — подмодули. Проекту остаётся реализация
/// `ImageSource` в `bsp` — см. модуль.
pub mod ota;

/// Узел `CPU_LOAD`: периодически публикует загрузку процессора; арифметика —
/// подмодуль — см. модуль.
pub mod cpu_load;

/// Узел `CANOPEN`: узел CANopen на шине CAN — стек `zencan` — см. модуль.
pub mod canopen;

/// Узел `HOST`: сообщения protobuf от хоста по кадровому каналу — разбор,
/// обработчик приложения, ответ — см. модуль.
pub mod host;

/// Modbus: slave над портом `ports::Registers` и master — поверх
/// транспортов `ports::ModbusServer`/`ports::ModbusClient` — см. модуль.
pub mod modbus;
