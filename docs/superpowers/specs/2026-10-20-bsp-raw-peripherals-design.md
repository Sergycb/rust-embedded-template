> **Заменено 2026-10-22:** ветка `refactor/bsp-levers` — `bsp` отдаёт рычаги платы, узлы собирают адаптеры через `ports::Assemble`.

# Плата отдаёт периферию, приложение собирает адаптеры

Дата: 2026-10-20 · после этапа 3 `feat/multi-transport`

## Зачем

Сейчас `Board` отдаёт готовые адаптеры: транспорт протокола с кадрированием,
буферами и сокетом (`bsp::link`), сетевой стек (`bsp::net`), USB-устройство с
классом CDC (`bsp::usb`), хранилище настроек (`bsp::settings`), адаптер OTA
(`bsp::ota`). Из-за этого `bsp` решает то, что плате не принадлежит: размер
буфера, число сокетов, порт протокола, кадрирование, VID/PID. Захотело
приложение другой буфер — правится `bsp`, хотя плата не менялась.

Решение пользователя: **плата отдаёт периферию, а не адаптеры.** Объекты
`embedded-hal`/`embedded-io-async`/`embassy-*-driver`, а для того, у чего
стандартного трейта нет (DAC, CAN async, EEPROM), — обёртку над трейтом из
`ports`. Адаптеры собирает `app` — composition root: в выражениях
`fragments:` графа и в своих модулях, где заводятся статики.

## Правила после работы

1. **`bsp` отдаёт то, что есть на плате**: шины и блоки (UART, CAN, Eth,
   USB-драйвер, RNG), разделы флеша по `memory.x`, сторож, сброс, счётчик
   тактов, `ChipSpec`. Ни буферов, ни сокетов, ни стека, ни классов USB.
2. **Буфер заводится там, где создаётся объект, который его держит** — в
   `app`. Размер — решение приложения.
3. **Аппаратная настройка — в `bsp`** (скорость UART, битрейт CAN, выводы,
   MAC из `uid`): это свойство платы. **Протокольная — в `app`/`domain`**
   (кадрирование, порт, таймауты, число сокетов).
4. **Зависимости**: `domain → ports` (+ `embassy-net`, `embassy-usb`,
   `embassy-time`, кодеки протоколов — так и остаётся, решение
   пользователя); `adapters → ports`; `bsp → ports` + HAL + трейты драйверов
   (`embedded-io-async`, `embedded-storage(-async)`, `embassy-net-driver`,
   `embassy-usb-driver`); `app → bsp, domain, adapters`. **`bsp` от
   `adapters`, `embassy-net`, `embassy-usb` не зависит.** `domain` от
   `adapters` не зависит (сейчас тоже — только ссылки в документации).
5. **В `graph.rs` — только конструирование и связывание**: `Rtu::new(board.usart1)`
   во входе фрагмента допустим, логика и статики — нет (статики — в модулях
   `app`, см. ниже).
6. **Без графа `main.rs` не раздувается**: каждая задача и каждый пул задач
   (`#[embassy_executor::task(pool_size = N)]`) — свой модуль в `app`: там
   задача-обёртка, сборка её адаптеров и их статики. `main.rs` только
   поднимает исполнитель, берёт `Board` и вызывает спавн этих модулей.
7. Задачи embassy не generic — псевдонимы составных типов
   (`type Usart1Link = adapters::modbus::Rtu<bsp::serial::usart1::Serial>`)
   пишет `app`, а не `bsp`.

## Что куда переезжает

| Сейчас | После |
|---|---|
| `bsp::link` (кадрирование, буферы `HOST_*`, сокеты, `PORT`) | `app/src/link.rs` — тот же Liquid-цикл по `links`, статики там же |
| `bsp::net`: `StackResources`, `embassy_net::new`, `SOCKETS` | `app/src/net.rs`; `bsp::net` отдаёт `Device` (драйвер Eth или заглушку) |
| `bsp::usb`: `Builder`, `CdcBuffers`, `State`, VID/PID, `product` | `app/src/usb.rs`; `bsp::usb::<имя>` отдаёт `UsbDriver` |
| `bsp::rng::seed` → `u64` в `Board::new` | `Board.rng` — объект RNG; затравку стека берёт `app` |
| `bsp::ota::Updater` (`adapters::ota::Updater` над разделами) | `bsp::ota` отдаёт разделы `Dfu`, `State` и `ACTIVE_LEN`; `Updater::new` — в `app` |
| `bsp::settings::Settings` (`adapters::settings::Settings` над `CONFIG`) | `bsp::settings` отдаёт раздел `CONFIG` как `NorFlash` (диапазон `0..capacity`); `Settings::new` — в `app` |
| `adapters::{serial,can,usb,net,flash}::Unconnected` | в `bsp` (решение пользователя: заглушка — «железо, которого пока нет», факт платы) |
| `adapters::flash::ErasePage` | в `bsp::flash` (геометрия флеша; её берёт и `boot`) |
| `adapters::net::mac_from_uid` | в `bsp::net` (MAC — свойство платы) |
| `adapters::eeprom::EepromAccess` (трейт, реализует `bsp`) | трейт — в `ports`; обёртка `Eeprom` остаётся в `adapters`, реализация доступа — в `bsp` |
| `embassy_stm32::uid::uid_hex()` в `bsp::usb` | `ChipSpec.uid_hex: &'static str` — серийный номер USB берёт `app` |

Остаётся в `bsp` как есть: `flash` (`FlashMutex`, `Whole`, банки), `external`,
`wdg` (`HardwareWatchdog` — трейт `watchdog`, не адаптер), `cpu::Reboot`,
`cycles`, `fault`, `cache`, `dma_nc`, `clocks`, `banks`, `panic`, `resources`.
`embassy-boot` у `bsp` остаётся ради `FirmwareUpdaterConfig::from_linkerfile`
(раскладка из `memory.x` — факт платы).

`boot` — второй composition root: зависит от `adapters` напрямую, как сейчас
при `boot_partitioned`/`ota=external`; `ErasePage` берёт из `bsp::flash`.

## Чего это стоит

- **Код не исчезает, а переезжает** из `bsp` в `app`: Liquid по `links` для
  транспортов окажется в `app/src/{link,net,usb}.rs`. Выигрыш — граница, а не
  строки.
- **Host-тесты заглушек и `ErasePage`** уходят вместе с ними из `adapters`
  (`bsp` собирается только под target). `ErasePage` — нетривиальная логика:
  её проверку перенести в `target-tests`.
- **`AGENTS.md`, `docs/architecture.md`, `docs/modules/bsp-*.md`** —
  переписать правила: «`Board` отдаёт адаптеры», «транспорт протокола плата
  отдаёт готовым», «буферы и сокет в `app` не заводят, статики — дело
  `bsp`», «`bsp` зависит от `ports` и `adapters`».
- Хук и `cargo-generate.toml`: списки удаляемых файлов (`bsp/src/link.rs`,
  `rng.rs` и др.) переезжают на файлы `app`.

## Этапы

Каждый — отдельный коммит, зелёный по четырём командам `AGENTS.md` и
`template-check` на конфигурациях с затронутым транспортом.

1. **Заглушки, `ErasePage`, `mac_from_uid`, `EepromAccess`** — переезд в
   `bsp`/`ports`; `boot` — на `bsp::flash::ErasePage`. Поведение не меняется.
2. **UART** (пилот): `Board.<uart>` — поток байтов; `app/src/link.rs`
   собирает `Rtu`/`Cobs` с буферами; `bsp/src/link.rs` теряет UART-ветки.
3. **CAN**: то же для `CANOPEN_FRAG` (вход — шина платы, без изменений по сути).
4. **USB**: `bsp::usb` отдаёт драйвер; `Builder`, CDC, VID/PID — в `app`.
5. **Eth/TCP/UDP + RNG**: стек, сокеты, `SOCKETS` — в `app`; `Board.rng`.
   После этапа `bsp/src/link.rs` удаляется.
6. **Флеш**: OTA и настройки — разделы из `bsp`, адаптеры в `app`.
7. **Без графа** (`graph=no`): то же, что граф, но по модулю на задачу или
   пул задач в `app` (правило 6); в `main.rs` остаются исполнитель, `Board`
   и вызовы спавна.
8. **Документация и инварианты** (`AGENTS.md`, `docs/`), зависимость
   `bsp → adapters` удаляется из `Cargo.toml` окончательно.

## Порядок относительно `feat/multi-transport`

Этап 3 нескольких транспортов идёт параллельно и правит те же файлы
(`bsp/src/{link,net,usb,serial,can,board}.rs`, `app/src/graph.rs`). Эта работа
начинается **после того, как этап 3 закоммичен**, поверх него: Liquid-циклы по
`links` переносятся в `app` как есть, а не пишутся заново.

## Не цели

- Выводы по умолчанию (распиновка) — по-прежнему заглушки.
- Перенос `embassy-net`/`embassy-usb` из `domain` (решение пользователя:
  оставить).
- Новые порты для периферии, которой в шаблоне нет (DAC и т. п.) — по правилу
  выше их заводит проект, когда появится потребитель.
