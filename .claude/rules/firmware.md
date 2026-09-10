---
paths: crates-cross/**
---

- Задачи embassy не могут быть generic: объект, уезжающий в
  `#[embassy_executor::task]`, обязан иметь псевдоним типа в `bsp`.
- Ровно один `#[panic_handler]` и один `#[global_logger]` на бинарник — это
  ограничение линкера, а не Cargo. Развилка по профилю делается через
  `#[cfg(debug_assertions)]` на `use ... as _;`, не Cargo-фичей.
- `Board` отдаёт объекты, реализующие порты, а не `Peripherals` и не пины.
  Исключения перечислены в `docs/architecture.md` — новых не заводить.
- Логирование только `defmt`; `log` в прошивке не используется.
- Файлы содержат Liquid и до генерации не являются валидным Rust: правки
  проверяются `template-check`, а не `cargo build`.
