# Хвосты после ревью `refactor/dispatch-ctx`

Заметки для следующих задач. Ревью ветки перед слиянием в `master`
(`5397197..HEAD`) нашло Critical/Important, они исправлены; здесь — то, что
осознанно оставлено на потом.

OTA wire-формат B (`Request.command = Ota { command: Start/Chunk/Finish/Reboot }`)
— окончательный: шаблон для новых проектов, совместимость со старыми клиентами
не предусматривается.

## 1. `unreachable!()` в OTA-логике

`crates-host/domain/src/ota/logic.rs`:

- `State::on_command` — `OtaCommand::Reboot => unreachable!`, потому что
  `Reboot` разбирается в цикле до разводки;
- `ota.rs` — `let State::Receiving(s) = state else { unreachable!("срок только
  у Receiving") }`.

Сегодня инварианты держатся, но второй вызов `on_command` или рефакторинг
превратит команду протокола в панику и сброс МК. Сделать состояние
непредставимым: отдельный `enum Step { Transition(Command), Reboot }` на входе
`on_command` или возврат `Outcome::Reboot` из разводки; тест, что `Reboot` не
доходит до `on_command`.

## 2. Голодание `ASK` при потоке `CMD`

`Ota::command` опрашивает `CMD` первым и возвращается сразу, как только там
есть запрос; `ASK` — только если `CMD` пуст. Клиент `CMD` не держит больше
одного запроса, но может переотправлять его сразу после ответа. Поток дешёвых
`CMD`-команд (например, `Start` с неверной подписью, которому узел отвечает без
флеша) может надолго заморить CANopen-клиент: его команда стоит в `Object`, а
sub 5 остаётся `0`.

Это перенесённое существующее поведение, не регрессия ветки. Варианты:
чередовать приоритет между кругами, опрашивать `ASK` первым при
`Object.in_flight`, или явно задокументировать и закрыть тестом.

## 3. Сквозной тест хэша пароля из `post-script.rhai`

`post-script.rhai` считает SHA-256 на Rhai; FIPS-вектор проверяет только ядро
хэша. Связка «соль-строка → байты константы» держится на неявной семантике
Rhai (итерация строки даёт `char`, `to_int` — кодовую точку). Обновление
Rhai/cargo-generate или правка соли может тихо запереть консоль.

Добавить тест в `chip-data-gen`: для образцового имени проекта посчитать хэш
той же функцией и сверить с `sha2`/`domain::console::PasswordHash`; либо
self-check в `post-script.rhai` (сравнить байты соли с `salt.bytes()`).

## 4. Валидация схемы в `crates-host/proto/build.rs`

Dispatch строится по имени RPC: `Some(Request_::Command::<Method>)`; считается,
что имя метода — PascalCase имени поля `oneof command`. Соглашение описано в
`host.proto`, но не проверяется:

- RPC с другим именем (`rpc ReadInfo(Get)`) даёт не компилирующийся
  generated-код с ошибкой в `OUT_DIR`;
- два сервиса с методом одного имени дают две одинаковые ветки — второй
  обработчик молча недостижим (`unreachable_patterns` ловится только
  `lint cross`).

Добавить в `build.rs` проверку по дескриптору: имя метода есть вариант
`Request.command`, имена уникальны; при расхождении — `panic!` с текстом про
соглашение. Заодно печатать `rerun-if-changed` для импортируемых `.proto`
(сейчас перечислены только `PROTOS`). Фильтр «только целевые файлы» уже
добавлен.

## 5. Одиночные `Ota*`-имена (не семейства)

Оставлены осознанно, переименование не входило в задачу:

- `crates-host/xtask/src/firmware.rs` — `OtaImage` (вариант `Image` ловит
  `clippy::struct_field_names` из-за поля `image`);
- `crates-host/domain/src/modbus/ota.rs` — `OtaRegisters` (имя `Registers`
  конфликтует с одноимённым трейтом);
- сгенерированные `OtaCtx`, `OtaWiring`, `OTA_FRAG` — имена узлов из макроса;
- `OtaService` и `Ota_::Start/Chunk/...` — имена proto-схемы.

## 6. Гейт перед слиянием

- [ ] Полный `template-check` (не только `--quick`) на финальном HEAD.
- [x] `cargo xtask lint`, `cargo xtask test host`.
- [ ] Сгенерированные конфигурации с `swap_paged` (F1/F3/L4/G4) — `cargo fmt --check`
      в `crates-cross`; исправление `//{{newline}}` уже проверено на F303RE.
- [x] SSH без OTA и SSH + OTA — `lint cross`.
- [x] Проект с консолью — сборка `app` проходит с ненулевым
      `console_password.rs` (проверено на SSH без OTA).
