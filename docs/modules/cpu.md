Чип: единственная точка, где поднимается HAL, и `Cpu` — всё, что чип отдаёт
плате.

`cpu::init()` поднимает HAL, включает кэши, разбирает распиновку
(`cpu/src/wiring.rs`, `assign_resources!`) и возвращает `cpu::Cpu` с
`CpuInfo`. Разбирает его на части `ports::Cpu::split` — кортеж в порядке полей
`bsp::Board`: сведения о чипе, сырые ручки флеша (`cpu::flash::Flash`,
`cpu::external::ExtFlash` — `Copy`), периферия транспортов в порядке `links`,
Ethernet, `cpu::system::System`, сторож, затравка сети.

- `cpu` знает `embassy-stm32` и не знает раскладки памяти: разделы и настройки
  строит `app` (`app::storage`), а `cpu::flash`/`cpu::external` отдают только
  сырые ручки.
- Свои замки крейт заводит на `ThreadModeRawMutex`: флеш, внешняя флешь и
  сторож живут в статиках `cpu`, а `app`/`boot` называют метку одним alias
  (`app::Mutex`).
- Границы MPU-регионов под буферы DMA `cpu::dma_nc` берёт числами из
  генерации: линкерных символов `memory.x` он не читает.

Модули: `flash`, `external`, `system`, `wdg`, `rng`, `com`, `can`, `usb`
(драйвер), `eth` (устройство), `cache`, `banks`, `clocks`, `dma_nc`, `wiring`.
