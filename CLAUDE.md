@AGENTS.md

## Claude Code

Ниже — то, что верно только для Claude Code, а не для агентов, следующих
одному лишь `AGENTS.md`.

- **Код — через `codebase-memory-mcp`.** Определения, вызовы, зависимости между
  крейтами — `search_graph`/`trace_path`/`get_code_snippet`/`query_graph`, а не
  Grep/Glob; проект не проиндексирован — сначала `index_repository`. Это касается
  и `crates-cross/**`: в сгенерированном проекте там обычный Rust.
  **Исключение — репозиторий самого шаблона** (признак — каталог `chip-data-gen/`
  в корне): файлы `crates-cross/**` там содержат Liquid (`{{ }}`, `{% %}`), дерево
  разбора для них не строится, и графа по ним нет — только Read/Grep.
- **Скиллы — до работы, а не по памяти.** Если установлены: `rust-engineering` —
  на любую правку Rust (общие идиомы и embedded, `AGENTS.md` их не дублирует);
  `rust-lib` — на всё, что трогает `supervisor_fragment!`/`supervisor_graph!`,
  `#[fsm::typestate]`, `watchdog`, `sync-request`.
- **Плата — через MCP, но с разрешения.** Запрет из `AGENTS.md` («не прошивать
  без явного разрешения») распространяется и на `embedded-debugger` (прошивка,
  сброс, запись памяти, RTT) и `serial`. Разрешение получено — работать ими, а не
  CLI `probe-rs`: лог — `rtt_attach`/`rtt_read`, панику и HardFault —
  `diagnose_fault`/`unwind_exception`.
