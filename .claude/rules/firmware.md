---
paths: crates-cross/**
---

Правила целиком — `AGENTS.md`, «Граница `domain`/`cross`» и «Инварианты,
которые не ловит компилятор»; здесь — что проверить при правке прошивки.

- Тип, уезжающий в `#[embassy_executor::task]`, — псевдонимом или конкретным
  типом: задачи не generic — `docs/architecture.md`.
- Паникёр у каждого бинарника свой, профиль — `#[cfg(debug_assertions)]`, не
  Cargo-фича — `docs/diagnostics.md`, «Два паникёра, один на сборку».
- `Board` отдаёт рычаги платы и `bsp::ChipSpec`, не периферию и не протоколы —
  `docs/architecture.md`, «`Board` отдаёт рычаги платы, а не периферию и не
  протоколы».
- Куда класть код и кто держит память адаптера — `AGENTS.md`, «Граница
  `domain`/`cross`».
- `bsp` ничего не запускает сверх подъёма HAL, конструкторы `new` только
  создают объекты — `docs/clocks.md`.
- `app` не знает `embassy-stm32`; паника и фолт — в `app` — `docs/clocks.md`,
  «Ядро: фолты и ловушка деления».
- `bsp` не зависит от `domain`{% if ota == "true" %}; `boot` зовёт из него только
  `domain::ota::boot::screen_update`{% endif %} — `docs/architecture.md`.
- `lib.rs` только объявляет — `docs/conventions.md`, «`lib.rs` только объявляет
  модули».
- Логирование только `defmt`.
- В репозитории самого шаблона (признак — `chip-data-gen/` в корне) файлы
  здесь содержат Liquid: проверка — `template-check`, уровень — `MAINTAINING.md`,
  «Какой уровень когда», а не `cargo build`. В сгенерированном проекте — обычный
  Rust.
