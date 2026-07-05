# rust-embedded-template

`cargo-generate` шаблон для embedded-проектов на STM32 (Rust, `no_std`, [Embassy](https://embassy.dev)),
с трёхэтапной моделью тестирования и единой точкой входа для сборки/тестов через `xtask`.

## Генерация проекта

```sh
cargo generate --git https://github.com/Sergycb/rust-embedded-template
```

Шаблон спросит:

- `chip` — идентификатор чипа для `probe-rs` (например `STM32F407VETx`);
- `chip_feature` — фича `embassy-stm32` для этого чипа (например `stm32f407ve`);
- `target` — целевой target triple (`thumbv7em-none-eabihf` и т.п.);
- `cpu` — `target-cpu` для rustflags (`cortex-m4` и т.п.);
- `ci` — CI-провайдер сгенерированного проекта: `github` / `gitlab` / `none`.

После генерации нужно вручную заполнить `MEMORY {}` в `cross/*/memory.x` — точные адреса и
размеры flash/RAM/bootloader-разделов зависят от конкретного чипа и от того, сколько места вы
хотите отвести под bootloader и DFU-раздел.

## Структура проекта

Два независимых Cargo workspace, потому что `cross` собирается под другой target и не может
делить `Cargo.lock`/профили с host-ориентированной частью:

```
├── domain/            # Бизнес-логика, no_std. Корневой workspace.
│   ├── adapters/       # Реализации портов
│   └── ports/          # Трейты-границы домена
├── host-target-tests/  # Тесты, гоняющие хост против прошитого устройства
├── xtask/              # Единая точка входа для сборки/тестов/флешинга
├── Cargo.toml           # Корневой workspace: domain, adapters, ports, xtask, host-target-tests
│
└── cross/               # Отдельный workspace, aппаратная часть (thumbv7em-none-eabihf и т.п.)
    ├── app/              # Основная прошивка (bin)
    ├── boot/             # Bootloader (bin, embassy-boot)
    ├── bsp/               # Board support package (lib)
    ├── target-tests/     # Тесты, исполняемые прямо на устройстве
    └── Cargo.toml
```

`domain` не входит в workspace `cross` (у него другой target), а подключается туда как обычная
path-зависимость. Версии зависимостей у `root` и `cross` объявлены раздельно и намеренно не
реэкспортируются друг через друга — `domain` не должен быть фасадом для инфраструктурных крейтов
вроде `static_cell`/`heapless`, которые `cross` использует для оркестрации задач. Синхронизацию
версий между двумя `[workspace.dependencies]` со временем берёт на себя бот, выбранный через
`ci` (Dependabot на GitHub, Renovate на GitLab — см. ниже); `post-script.rhai` дополнительно
обновляет `Cargo.lock` сразу при генерации, не дожидаясь первого PR от бота.

## Трёхэтапное тестирование

| Этап | Где выполняется | Команда |
|---|---|---|
| host | обычный `cargo test`, без железа | `cargo xtask test host` |
| target | на устройстве, `embedded-test` harness | `cargo xtask test target` |
| host-target | хост управляет прошитым устройством | `cargo xtask test host-target` |

`cargo xtask test all` прогоняет все три этапа подряд.

## Команды `xtask`

```
cargo xtask build                  # cross: сборка app+boot в debug и release
cargo xtask run [debug|release]    # прошить boot, запустить app через probe-rs run
cargo xtask flash [debug|release]  # прошить boot+app без подключения дебаггера
cargo xtask lint [cross]           # fmt --check + clippy -D warnings (host или cross)
cargo xtask deny                   # cargo-deny check (лицензии/уязвимости/дубли версий)
cargo xtask test [all|host|host-target|target]
```

## debug vs release: defmt vs log

`cross/app` и `cross/boot` — взаимоисключающие Cargo-фичи `debug` (defmt + RTT + `panic-probe`) и
`release` (`log` + `panic-abort`); при отсутствии обеих или при обеих сразу сборка не пройдёт —
это специально проверяется `compile_error!` в начале `main.rs`. `domain` и `bsp` не выбирают
профиль сами, а только прокидывают одноимённые фичи (`defmt`/`log`) дальше по зависимостям.

## Обновление зависимостей

`ci` определяет не только CI-workflow, но и бота для PR с обновлениями версий:
`github` → `.github/dependabot.yml`, `gitlab` → `renovate.json`, `none` → ни того, ни другого.
Оба покрывают обе `[workspace.dependencies]` (корневую и `cross/`).

## Инструменты

Версия Rust и компоненты (`clippy`, `rustfmt`) зафиксированы в `rust-toolchain.toml` (корень —
host-тулчейн, `cross/rust-toolchain.toml` — тот же тулчейн + целевой `target`, `rustup` поставит
его автоматически). Для прошивки/отладки нужны `probe-rs` и `flip-link`:

```sh
cargo install probe-rs-tools flip-link
```

В `.vscode/` — задачи, привязанные к `cargo xtask` (`tasks.json`), запуск отладки через
`probe-rs` (`launch.json`, `F5`) и список рекомендуемых расширений (`extensions.json`).
