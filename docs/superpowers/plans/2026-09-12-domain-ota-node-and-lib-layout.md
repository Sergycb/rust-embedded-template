# Узел `OTA` в `domain` и правило `lib.rs` — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ports` разбит на модули по правилу «`lib.rs` только объявляет», а в `domain` появляется узел `OTA` (`ota::run`/`run_signed` + фрагменты), который сам владеет каналом и применяет образ; проекту остаётся реализовать `ImageSource` в `bsp`.

**Architecture:** `ImageSource` становится полным протоколом (`begin` → `next`… → `finish`), `SignedFirmwareUpdate` отличает плохую подпись от отказа флеша (`VerifyError`), а `domain::ota` крутит цикл над обоими портами и сообщает отправителю плоский код отказа `Rejection`. Фрагменты `OTA_FRAG`/`OTA_SIGNED_FRAG` объявляют собственную `boot:`-привязку `Inputs<OtaLink, OtaFlash>`; `graph.rs` кормит её полями `Board` и выбирает фрагмент по плейсхолдеру `signed`. `bsp` отдаёт заглушку канала `Link`, чтобы `template-check` раскрывал фрагменты в обоих вариантах.

**Tech Stack:** Rust 2024, `no_std`, Embassy, `supervisor` (rust-lib, `supervisor_fragment!` с `boot:`), `embassy-boot` 0.7, `defmt-or-log`, `cargo nextest`, `cargo-generate`.

**Spec:** `docs/superpowers/specs/2026-09-12-domain-ota-node-and-lib-layout-design.md`

## Global Constraints

- Liquid (`{{ }}`, `{% %}`) в `crates-host/domain`, `ports`, `adapters` запрещён — эти крейты компилируются в самом репозитории шаблона. Liquid только в `crates-cross/**` и документации, которая туда `include_str!`-ится.
- `crates-cross` в репозитории шаблона не собирается (Liquid в исходниках). Проверка — только генерацией (`cargo generate` → `cargo xtask lint cross` → `cargo xtask build`) с отдельным `CARGO_TARGET_DIR` на вариант.
- Перед каждым коммитом с правками `crates-host`: `cargo xtask lint` и `cargo xtask test host` — оба `EXIT=0`. Запускать из корня репозитория.
- Пути `ports::FirmwareUpdate`, `ports::ImageSource`, `ports::SettingsStorage`, `ports::DownloadError`, `ports::UpdateError`, `ports::SignedFirmwareUpdate` должны остаться рабочими (реэкспорт из корня `ports`).
- Новые типы и имена — ровно те, что в спеке: `ports::download::{Announce, Rejection}`, `ports::update::VerifyError`, `UpdateError::BadSignature`, `domain::ota::{Inputs, run, run_signed}`, фрагменты `OTA_FRAG`/`OTA_SIGNED_FRAG`, имена compose-site `OtaLink`/`OtaFlash`, `bsp::ota::Link`, поле `Board::ota_link`.
- Коммиты — на русском, в стиле репозитория (`refactor(ports): …`, `feat(domain): …`, `docs: …`), с trailer `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
- Плату не трогать: ни `flash`, ни `test target`, ни `probe-rs`.
- Не менять `memory.x`, линкер-скрипты, `.cargo/config.toml`.

---

## Карта файлов

| Файл | Что с ним |
|---|---|
| `crates-host/domain/ports/src/lib.rs` | остаётся: атрибуты, `//!`, три `pub mod`, `pub use` |
| `crates-host/domain/ports/src/update.rs` | **новый**: `FirmwareUpdate`, `SignedFirmwareUpdate`, `UpdateError`, `VerifyError` |
| `crates-host/domain/ports/src/download.rs` | **новый**: `ImageSource`, `Announce`, `Rejection`, `DownloadError` |
| `crates-host/domain/ports/src/settings.rs` | **новый**: `SettingsStorage` |
| `crates-host/domain/src/lib.rs` | `test_support` → файл; `pub mod ota` |
| `crates-host/domain/src/test_support.rs` | **новый**: `block_on`, `FakeFlash`, `FakeLink`, константы |
| `crates-host/domain/src/download.rs` | тесты на общие фейки |
| `crates-host/domain/src/update.rs` | `VerifyError` → `UpdateError::BadSignature`; тесты на общие фейки |
| `crates-host/domain/src/ota.rs` | **новый**: `Inputs`, `run`, `run_signed`, цикл, фрагменты, тесты |
| `crates-host/domain/adapters/src/ota.rs` | `Signed::verify_and_mark_updated` → `VerifyError`; тест |
| `Cargo.lock` | `cargo update -p supervisor` |
| `crates-cross/bsp/src/ota.rs` | заглушка `Link: ImageSource` |
| `crates-cross/bsp/src/board.rs` | поле `ota_link` |
| `crates-cross/app/src/graph.rs` | `OtaLink`/`OtaFlash`, `fragments:` с OTA |
| `crates-cross/app/src/main.rs` | комментарий про заборы из `board` |
| `docs/conventions.md` | правило `lib.rs` |
| `docs/architecture.md` | узел OTA; типы на compose-site; `Board.ota_link` |
| `docs/modules/bsp-ota.md`, `docs/modules/app-graph.md` | транспорт = `ImageSource` в `bsp`; узел вместо ручного цикла |
| `docs/ota.md`, `README.md`, `docs/README.md`, `AGENTS.md` | поток через узел, `VerifyError` |

---

### Task 1: `ports` — три модуля, `lib.rs` только объявляет

**Files:**
- Modify: `crates-host/domain/ports/src/lib.rs`
- Create: `crates-host/domain/ports/src/update.rs`
- Create: `crates-host/domain/ports/src/download.rs`
- Create: `crates-host/domain/ports/src/settings.rs`
- Modify: `docs/conventions.md` (новый раздел перед «Известные, осознанные ограничения»)

**Interfaces:**
- Consumes: текущий `ports/src/lib.rs` (286 строк): трейты `FirmwareUpdate` (стр. 30–136), `SignedFirmwareUpdate` (138–165), `ImageSource` (167–181), `SettingsStorage` (183–203), `DownloadError` (205–241), `UpdateError` + `impl From` (243–286).
- Produces: модули `ports::update`, `ports::download`, `ports::settings`; все прежние пути через `pub use` в корне.

- [ ] **Step 1: Создать `ports/src/update.rs`**

Заголовок модуля и перенесённые без изменений строки 30–165 и 243–286 старого `lib.rs` (doc-комментарии переезжают вместе с кодом):

```rust
//! Порты обновления прошивки и ошибки его применения.
//!
//! [`FirmwareUpdate`] — то, что нужно приёму и применению образа без подписи;
//! [`SignedFirmwareUpdate`] — то же плюс проверка подписи. Реализует их
//! `adapters::ota` (`Updater` и `Signed`), пользуются — `domain::download`,
//! `domain::update` и узел `domain::ota`.

// --- сюда строки 30–165 старого lib.rs: FirmwareUpdate и SignedFirmwareUpdate ---

// --- сюда строки 243–286 старого lib.rs: UpdateError и impl From ---
```

Ни одного изменения в перенесённом коде на этом шаге. Ссылки вида
[`prepare`](Self::prepare) внутри трейта резолвятся как и раньше.

- [ ] **Step 2: Создать `ports/src/download.rs`**

```rust
//! Порт канала доставки образа и ошибка его приёма.
//!
//! [`ImageSource`] реализует проект под свой транспорт (USB CDC, UART, сеть,
//! SD-карта) — в `crates-cross/bsp`, там, где живёт периферия. Пользуется им
//! `domain::download::receive` и узел `domain::ota`.

// --- сюда строки 167–181 старого lib.rs: ImageSource ---

// --- сюда строки 205–241 старого lib.rs: DownloadError ---
```

- [ ] **Step 3: Создать `ports/src/settings.rs`**

```rust
//! Порт хранилища настроек.
//!
//! Реализует `adapters::settings::Settings`, отдаёт полем `Board::settings`
//! проект с разделом `CONFIG`; формат значений выбирает проект.

// --- сюда строки 183–203 старого lib.rs: SettingsStorage ---
```

- [ ] **Step 4: Переписать `ports/src/lib.rs`**

Полное новое содержимое (строки 1–28 старого файла сохраняются дословно, дальше — только объявления):

```rust
#![cfg_attr(not(test), no_std)]
// `async fn` в публичном трейте: rustc предупреждает (`async_fn_in_trait`), что
// у возвращаемого future нет границы `Send` и потребовать её вызывающий не
// сможет. Для однопоточного исполнителя embassy это ровно то, что нужно, а с
// `-D warnings` предупреждение стало бы ошибкой сборки. Так же поступает
// `embedded-hal-async` (его lib.rs, строка 10).
#![allow(async_fn_in_trait)]

//! Порты — границы `domain`: трейты, которые логика вызывает, не зная, что за
//! ними стоит, и типы, которыми она с ними разговаривает. Реализации живут в
//! `adapters` (всё, что generic по `embedded-hal`/`embedded-storage` и потому
//! тестируется на хосте) и в `crates-cross/bsp` (то, чему нужна периферия
//! конкретного чипа), а сама логика ни о тех, ни о других не знает.
//!
//! Правило, по которому решается, что сюда класть: если для проверки логики
//! нужно железо — значит между логикой и железом не хватает порта. Обратное
//! правило не менее важно: порт заводится под то, чем пользуется логика, а не
//! под каждую железную деталь. Причина прошлой паники домену не нужна — это
//! диагностика старта, и её печатает `main` сам.
//!
//! Типы ошибок тоже здесь, а не в `domain`: домен — только функции над портами,
//! а всё, что он возвращает, объявлено в его словаре.
//!
//! Трейты объявлены безусловно, потому что Liquid в этом крейте запрещён (он
//! компилируется и в самом репозитории шаблона): [`FirmwareUpdate`] и
//! [`SettingsStorage`] реализованы, когда при генерации выбраны OTA и раздел
//! настроек, [`SignedFirmwareUpdate`] — когда выбрана подпись, а [`ImageSource`]
//! реализует проект под свой канал доставки.
//!
//! Модули названы по операции домена, которая ими пользуется, а не по трейту:
//! ошибка операции (`DownloadError` у `domain::download::receive`) лежит рядом
//! с портом, через который операция идёт. Наружу всё реэкспортируется в корень
//! — путь `ports::FirmwareUpdate` короче, а внутреннее деление пользователя не
//! касается (`docs/conventions.md`, «`lib.rs` только объявляет модули»).

/// Обновление прошивки: запись образа, проверка подписи, ошибки применения.
pub mod update;

/// Канал доставки образа и ошибка приёма.
pub mod download;

/// Хранилище настроек.
pub mod settings;

pub use download::{DownloadError, ImageSource};
pub use settings::SettingsStorage;
pub use update::{FirmwareUpdate, SignedFirmwareUpdate, UpdateError};
```

- [ ] **Step 5: Проверить, что ничего не изменилось по смыслу**

Run: `cargo xtask lint`
Expected: `EXIT=0` (fmt чистый, clippy без предупреждений; `pub use` внутри крейта не даёт `unused_imports` — их используют `domain`/`adapters`).

Run: `cargo xtask test host`
Expected: `EXIT=0`, тот же набор тестов, что до правки.

- [ ] **Step 6: Записать правило в `docs/conventions.md`**

Вставить перед строкой `## Известные, осознанные ограничения (не баги)`:

```markdown
## `lib.rs` только объявляет модули

В корне крейта (`lib.rs`) лежат только атрибуты крейта (`#![no_std]`,
`#![allow(..)]` с обоснованием), `//!`-документация, объявления `mod` с
doc-комментарием у каждого и `pub use`-реэкспорты. Определений — типов,
трейтов, функций, `impl` — в нём нет: они живут в модулях, названных по
предметной области (`ports::update`, `domain::download`), а не по слою
(`types`, `utils`).

Реэкспорт в корне — часть правила, а не исключение из него: так устроены
`embedded-hal` и `embassy-*`, и так путь `ports::FirmwareUpdate` остаётся
коротким, а перестановка модулей внутри крейта не касается его пользователей.

Тестовые помощники — тоже модуль (`domain::test_support`), а не inline-блок
в `lib.rs`: фейки портов нужны нескольким модулям, и один файл на них короче
трёх копий.
```

- [ ] **Step 7: Commit**

```bash
git add crates-host/domain/ports/src docs/conventions.md
git commit -m "refactor(ports): три модуля по операциям домена, lib.rs только объявляет

Правило «lib.rs = атрибуты, //!, mod, pub use» записано в
docs/conventions.md. Реэкспорт в корень сохраняет все прежние пути
(ports::FirmwareUpdate и т.д.), поэтому потребители не меняются.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `domain::test_support` — файл и общие фейки портов

**Files:**
- Create: `crates-host/domain/src/test_support.rs`
- Modify: `crates-host/domain/src/lib.rs` (строки 7–23: inline-модуль → объявление)
- Modify: `crates-host/domain/src/download.rs` (тесты, строки 161–274: фейки уезжают)
- Modify: `crates-host/domain/src/update.rs` (тесты, строки 100–183: фейк уезжает)

**Interfaces:**
- Consumes: `ports::{FirmwareUpdate, SignedFirmwareUpdate, ImageSource}` в их текущем виде.
- Produces: `crate::test_support::{block_on, FakeFlash, FakeLink, KEY, SIGNATURE}`. `FakeFlash::new(capacity: usize, word: u32)`, `FakeFlash::with_image(len: u32, incoming: u32)`; публичные (`pub(crate)`) поля `memory`, `capacity`, `word`, `busy`, `prepared`, `writes`, `version`, `key`, `verify`, `verified`, `mark_updated`, `updated`. `FakeLink::of(chunks)`; поля `next`, `fail_at`.

- [ ] **Step 1: Написать `test_support.rs`**

```rust
//! Помощники host-тестов: прогон future без исполнителя и фейки обоих портов
//! обновления.
//!
//! Один фейк на порт, общий для `download`, `update` и `ota`: сценарии у
//! модулей разные, а придирки фейка — те же, что у настоящего адаптера
//! (кратность записи слову, запись только после `prepare`), и держать их в
//! трёх копиях значило бы проверять три разных флеша.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

use ports::{FirmwareUpdate, ImageSource, SignedFirmwareUpdate};

use crate::firmware::pack;
use crate::update::VERSION_BYTES;

/// Ключ, которым «подписаны» образы в тестах. Ненулевой: нули — «ключ не
/// подставлен», и логика отказывает до криптографии.
pub(crate) const KEY: [u8; 32] = [7; 32];
/// Подпись, которую тесты кладут в заголовок; фейк её не проверяет, а
/// записывает — важно, что до него доехала именно она.
pub(crate) const SIGNATURE: [u8; 64] = [9; 64];

/// Крутит future фейков в host-тестах: ни один порт-фейк не ждёт по-настоящему,
/// поэтому исполнитель здесь не нужен — достаточно одного `poll`.
pub(crate) fn block_on<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("фейки портов не ждут: future не должна возвращать Pending"),
    }
}

/// Канал, отдающий заранее нарезанные куски; на `fail_at`-м вызове `next`
/// отказывает — так разыгрывается обрыв связи.
pub(crate) struct FakeLink {
    chunks: Vec<Vec<u8>>,
    /// Сколько кусков уже отдано — по нему тест видит, читался ли канал.
    pub(crate) next: usize,
    pub(crate) fail_at: Option<usize>,
}

impl FakeLink {
    pub(crate) fn of(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            next: 0,
            fail_at: None,
        }
    }
}

impl ImageSource for FakeLink {
    type Error = &'static str;

    async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error> {
        if self.fail_at == Some(self.next) {
            return Err("обрыв канала");
        }
        let i = self.next;
        self.next += 1;
        Ok(self.chunks.get(i).map(Vec::as_slice))
    }
}

/// Флеш в памяти, придирчивый ровно там же, где настоящий, плюс всё, что
/// нужно применению с подписью: версия, ключ, исход проверки.
///
/// Главное здесь — проверки кратности: без них тест не отличил бы
/// работающую буферизацию от её отсутствия, а на плате разница вылезла бы
/// отказом записи (или, на F2/F4/F7, испорченным образом).
pub(crate) struct FakeFlash {
    pub(crate) memory: Vec<u8>,
    /// То, что отдаёт `capacity()`. По умолчанию — длина `memory`; тесты
    /// применения ставят меньше, чтобы разыграть «в приёмник влез, в
    /// активный раздел — нет».
    pub(crate) capacity: u32,
    pub(crate) word: u32,
    pub(crate) busy: bool,
    pub(crate) prepared: Option<u32>,
    pub(crate) writes: Vec<(u32, usize)>,
    pub(crate) version: u32,
    pub(crate) key: [u8; 32],
    pub(crate) verify: Result<(), &'static str>,
    pub(crate) verified: Vec<([u8; 64], u32)>,
    /// Что ответить на `mark_updated`; `Err` разыгрывает отказ флеша.
    pub(crate) mark_updated: Result<(), &'static str>,
    /// Сколько раз просили обмен разделов.
    // `cargo xtask lint` — это clippy `--all-targets -D warnings`: поле, которое
    // до задачи 5 никто не читает, уронило бы его. Снять в задаче 5.
    #[expect(dead_code, reason = "читают тесты узла OTA — задача 5")]
    pub(crate) updated: usize,
}

impl FakeFlash {
    pub(crate) fn new(capacity: usize, word: u32) -> Self {
        Self {
            memory: vec![0xFF; capacity],
            capacity: capacity as u32,
            word,
            busy: false,
            prepared: None,
            writes: Vec::new(),
            version: pack(1, 2, 3),
            key: KEY,
            verify: Ok(()),
            verified: Vec::new(),
            mark_updated: Ok(()),
            updated: 0,
        }
    }

    /// Раздел, в котором уже лежит образ длиной `len` с версией `incoming` в
    /// хвосте, и вместимость меньше раздела — как у настоящей схемы с обменом.
    pub(crate) fn with_image(len: u32, incoming: u32) -> Self {
        let mut flash = Self::new(256, 8);
        flash.capacity = 128;
        flash.memory[(len - VERSION_BYTES) as usize..len as usize]
            .copy_from_slice(&incoming.to_le_bytes());
        flash
    }
}

impl FirmwareUpdate for FakeFlash {
    type Error = &'static str;

    fn write_granularity(&mut self) -> u32 {
        self.word
    }

    fn capacity(&mut self) -> Result<u32, Self::Error> {
        Ok(self.capacity)
    }

    fn is_busy(&mut self) -> Result<bool, Self::Error> {
        Ok(self.busy)
    }

    fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
        if len == 0 || len > self.capacity {
            return Err("негодная длина");
        }
        self.prepared = Some(len);
        self.memory.fill(0xFF);
        Ok(())
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        if self.prepared.is_none() {
            return Err("запись без подготовки раздела");
        }
        if !(data.len() as u32).is_multiple_of(self.word) {
            return Err("длина записи не кратна слову");
        }
        if !offset.is_multiple_of(self.word) {
            return Err("смещение не кратно слову");
        }
        let start = offset as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
        self.writes.push((offset, data.len()));
        Ok(())
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        let start = offset as usize;
        buf.copy_from_slice(&self.memory[start..start + buf.len()]);
        Ok(())
    }

    fn mark_booted(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn mark_updated(&mut self) -> Result<(), Self::Error> {
        self.updated += 1;
        self.mark_updated
    }
}

impl SignedFirmwareUpdate for FakeFlash {
    fn running_version(&self) -> u32 {
        self.version
    }

    fn public_key(&self) -> &[u8; 32] {
        &self.key
    }

    fn verify_and_mark_updated(
        &mut self,
        signature: &[u8; 64],
        len: u32,
    ) -> Result<(), Self::Error> {
        self.verified.push((*signature, len));
        self.verify
    }
}
```

- [ ] **Step 2: `lib.rs` — объявление вместо inline-модуля**

Заменить строки 7–23 (`/// Крутит future …` до закрывающей `}` inline-модуля) на:

```rust
/// Прогон future без исполнителя и фейки портов для host-тестов — см. модуль.
#[cfg(test)]
pub(crate) mod test_support;
```

- [ ] **Step 3: `download.rs` — тесты на общие фейки**

В `mod tests` (строка 162 и далее):

1. Заменить импорты:
   ```rust
   use super::{MAX_WORD, receive};
   use crate::test_support::{FakeFlash, FakeLink, block_on};
   use ports::{DownloadError, FirmwareUpdate};
   ```
   (`ImageSource` больше не нужен — фейк канала реализован в `test_support`.)
2. Удалить определения `Chunks` (с doc-комментарием) и `FakeFlash` вместе с их `impl` — строки 167–274.
3. Во всех тестах `Chunks::of(` → `FakeLink::of(` (девять мест).

Обёртка `NoPrepare(FakeFlash)` в `passes_a_flash_failure_through` остаётся как есть — она зовёт только методы трейта.

- [ ] **Step 4: `update.rs` — тесты на общий фейк**

В `mod tests` (строка 100 и далее):

1. Импорты:
   ```rust
   use super::apply_signed;
   use crate::firmware::pack;
   use crate::test_support::{FakeFlash, SIGNATURE};
   use ports::UpdateError;
   ```
2. Удалить `KEY`, `SIGNATURE`, структуру `Fake` с обоими `impl` — строки 105–183.
3. Во всех тестах `Fake::with_image(` → `FakeFlash::with_image(` (семь мест). Поля `busy`, `key`, `verify`, `verified` называются так же — тела тестов не меняются.

- [ ] **Step 5: Проверить**

Run: `cargo xtask lint`
Expected: `EXIT=0`. Если clippy ругается на неиспользуемые поля `FakeFlash` (`dead_code` под `cfg(test)`) — поля `capacity`/`mark_updated`/`updated` уже читаются трейтом, `writes`/`prepared`/`next`/`fail_at` — тестами; неиспользуемых быть не должно.

Run: `cargo xtask test host`
Expected: `EXIT=0`, тот же набор тестов (17 в `download`+`update`), все зелёные.

- [ ] **Step 6: Commit**

```bash
git add crates-host/domain/src/lib.rs crates-host/domain/src/test_support.rs crates-host/domain/src/download.rs crates-host/domain/src/update.rs
git commit -m "refactor(domain): test_support — файл вместо inline-модуля, один фейк на порт

FakeFlash из download и Fake из update были двумя флешами с разными
придирками; узлу OTA нужны оба свойства сразу — запись словами и
проверка подписи. Один фейк, реализующий оба трейта, вместо трёх копий.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `VerifyError` — плохая подпись отличима от отказа флеша

**Files:**
- Modify: `crates-host/domain/ports/src/update.rs` (`SignedFirmwareUpdate::verify_and_mark_updated`, `UpdateError`, новый `VerifyError`)
- Modify: `crates-host/domain/ports/src/lib.rs` (реэкспорт `VerifyError`)
- Modify: `crates-host/domain/adapters/src/ota.rs` (`Signed`, тест)
- Modify: `crates-host/domain/src/update.rs` (`apply_signed`, тест `passes_a_signature_failure_through`)
- Modify: `crates-host/domain/src/test_support.rs` (поле `verify`)

**Interfaces:**
- Produces: `ports::VerifyError<E> { BadSignature, Flash(E) }`; `SignedFirmwareUpdate::verify_and_mark_updated(&mut self, signature: &[u8; 64], len: u32) -> Result<(), VerifyError<Self::Error>>`; `UpdateError::BadSignature`.

- [ ] **Step 1: Тест в `domain/src/update.rs` — сначала красный**

Заменить тест `passes_a_signature_failure_through` на два:

```rust
    /// Подпись не сошлась — свой вариант, а не `Flash(_)`: отправителю
    /// важно отличить «исправь ключ» от «устройство неисправно».
    #[test]
    fn reports_a_bad_signature_as_such() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.verify = Err(VerifyError::BadSignature);

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::BadSignature)
        );
    }

    /// Отказ флеша внутри проверки доходит как отказ флеша.
    #[test]
    fn passes_a_flash_failure_inside_verify_through() {
        let mut flash = FakeFlash::with_image(64, pack(1, 2, 4));
        flash.verify = Err(VerifyError::Flash("флеш отказал"));

        assert_eq!(
            apply_signed(&mut flash, &SIGNATURE, 64),
            Err(UpdateError::Flash("флеш отказал"))
        );
    }
```

и добавить в импорты тестов `use ports::{UpdateError, VerifyError};` (вместо `use ports::UpdateError;`).

- [ ] **Step 2: Убедиться, что не компилируется**

Run: `cargo xtask test host`
Expected: ошибка компиляции — `VerifyError` не найден.

- [ ] **Step 3: `ports/src/update.rs` — тип и сигнатура**

Добавить после `UpdateError` и его `impl From`:

```rust
/// Чем может закончиться проверка подписи в адаптере
/// ([`SignedFirmwareUpdate::verify_and_mark_updated`]).
///
/// Два варианта, а не один `Self::Error`: «подпись не сошлась» — отказ
/// образу, «флеш отказал» — отказ устройства, и отправителю образа нужно
/// разное — сменить ключ или прекратить попытки. Пока оба приходили одним
/// вариантом, узел OTA не мог сказать отправителю ничего точнее «не вышло».
#[non_exhaustive]
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum VerifyError<E> {
    /// Подпись не сошлась с ключом устройства — или ключ не разбирается.
    #[error("подпись не сошлась")]
    BadSignature,
    /// Отказ флеша при чтении раздела или пометке обмена.
    #[error("отказ адаптера обновления: {0:?}")]
    Flash(E),
}
```

В `UpdateError<E>` добавить вариант перед `Flash(E)`:

```rust
    /// Подпись не сошлась — отдельно от [`Flash`](Self::Flash): узел OTA
    /// сообщает отправителю код `Signature`, а не `Device`.
    #[error("подпись не сошлась")]
    BadSignature,
```

и поправить doc-комментарий у `Flash(E)`: `/// Всё, что приходит из адаптера: отказ флеша, негодное состояние.` (слова «неверная подпись» убрать).

Сигнатура метода в `SignedFirmwareUpdate`:

```rust
    /// Проверяет подпись SHA-512 первых `len` байт раздела обновления по
    /// хранимому ключу и, если она верна, помечает обмен разделов.
    ///
    /// Только криптография и пометка: длину, занятость раздела и версию
    /// проверяет логика ДО этого вызова, в том порядке, в каком отказы
    /// различимы (`domain::update`). Неверная подпись — своим вариантом
    /// [`VerifyError::BadSignature`], а не как отказ флеша.
    fn verify_and_mark_updated(
        &mut self,
        signature: &[u8; 64],
        len: u32,
    ) -> Result<(), VerifyError<Self::Error>>;
```

В `ports/src/lib.rs`: `pub use update::{FirmwareUpdate, SignedFirmwareUpdate, UpdateError, VerifyError};`.

- [ ] **Step 4: `domain/src/update.rs` — маппинг**

Заменить строку `flash.verify_and_mark_updated(signature, len)?;` на:

```rust
    flash
        .verify_and_mark_updated(signature, len)
        .map_err(|err| match err {
            VerifyError::BadSignature => UpdateError::BadSignature,
            VerifyError::Flash(err) => UpdateError::Flash(err),
        })?;
```

и импорт: `use ports::{SignedFirmwareUpdate, UpdateError, VerifyError};`.

- [ ] **Step 5: `test_support.rs` — тип поля `verify`**

`pub(crate) verify: Result<(), VerifyError<&'static str>>,` и импорт `use ports::{FirmwareUpdate, ImageSource, SignedFirmwareUpdate, VerifyError};`. Сигнатура `verify_and_mark_updated` в `impl SignedFirmwareUpdate for FakeFlash` — `-> Result<(), VerifyError<Self::Error>>`, тело прежнее.

- [ ] **Step 6: `adapters/src/ota.rs` — маппинг у `Signed`**

Метод `verify_and_mark_updated` в `impl ports::SignedFirmwareUpdate for Signed` (строки 288–295):

```rust
    /// Проверка читает весь раздел и считает по нему хеш — заметное время
    /// (портируемая реализация ed25519 — порядка сотни миллионов тактов).
    /// Случается это один раз перед перезагрузкой.
    ///
    /// `Signature(_)` у embassy-boot — и не сошедшаяся подпись, и ключ,
    /// который не разбирается как точка кривой; для отправителя это одно и
    /// то же «подпись негодная».
    fn verify_and_mark_updated(
        &mut self,
        signature: &[u8; 64],
        len: u32,
    ) -> Result<(), VerifyError<Self::Error>> {
        let key = self.public_key;
        self.inner
            .verify_and_mark_updated(&key, signature, len)
            .map_err(|err| match err {
                Error::Signature(_) => VerifyError::BadSignature,
                other => VerifyError::Flash(other),
            })
    }
```

Импорт в начале файла: `use ports::FirmwareUpdate;` → `use ports::{FirmwareUpdate, VerifyError};`.

Тест (рядом с `signed_mark_updated_always_refuses`, под тем же `#[cfg(feature = "signed")]`):

```rust
    /// Подпись, которая не может сойтись (ключ и подпись — константы), доходит
    /// до порта своим вариантом, а не как отказ флеша.
    #[cfg(feature = "signed")]
    #[test]
    fn signed_reports_a_bad_signature_as_such() {
        use ports::{SignedFirmwareUpdate, VerifyError};
        let mut signed = Signed::new(updater(), 1, [7; 32]);
        signed.prepare(64).expect("стереть под образ");
        signed.write(0, &[0xA5; 64]).expect("записать образ");

        assert!(matches!(
            signed.verify_and_mark_updated(&[9; 64], 64),
            Err(VerifyError::BadSignature)
        ));
        assert!(
            !signed.is_busy().expect("состояние"),
            "негодная подпись не должна помечать обмен"
        );
    }
```

- [ ] **Step 7: Проверить**

Run: `cargo xtask lint`
Expected: `EXIT=0`.

Run: `cargo xtask test host`
Expected: `EXIT=0`; новые тесты `reports_a_bad_signature_as_such`, `passes_a_flash_failure_inside_verify_through`, `signed_reports_a_bad_signature_as_such` зелёные.

- [ ] **Step 8: Commit**

```bash
git add crates-host/domain/ports/src crates-host/domain/adapters/src/ota.rs crates-host/domain/src/update.rs crates-host/domain/src/test_support.rs
git commit -m "feat(ports): VerifyError — неверная подпись отличима от отказа флеша

verify_and_mark_updated возвращал Self::Error, и BadSignature от
embassy-boot доезжал до домена как UpdateError::Flash. Узлу OTA нужно
сказать отправителю «подпись», а не «устройство», — отсюда свой вариант
у порта и у UpdateError.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `ImageSource` — `begin`/`finish`, `Announce`, `Rejection`

**Files:**
- Modify: `crates-host/domain/ports/src/download.rs`
- Modify: `crates-host/domain/ports/src/lib.rs` (реэкспорт)
- Modify: `crates-host/domain/src/test_support.rs` (`FakeLink`)

**Interfaces:**
- Produces: `ports::Announce { len: u32, signature: Option<[u8; 64]> }`; `ports::Rejection { Length, Transfer, Busy, Rollback, Signature, Device }`; `ImageSource::begin(&mut self) -> Result<Announce, Self::Error>`; `ImageSource::finish(&mut self, outcome: Result<(), Rejection>) -> Result<(), Self::Error>`. `FakeLink::announcing(self, announces)`; поля `finish: Result<(), &'static str>`, `finished: Vec<Result<(), Rejection>>`.

- [ ] **Step 1: `ports/src/download.rs` — типы и методы**

Перед `pub trait ImageSource` добавить:

```rust
/// Заголовок образа, который канал получил от отправителя, — то, что нужно
/// знать до первого куска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Announce {
    /// Длина образа в байтах. Сверяется с вместимостью ДО стирания раздела.
    pub len: u32,
    /// Подпись образа, если канал её передаёт. `None` в проекте с подписью —
    /// отказ до стирания (`Rejection::Signature`); проект без подписи поле
    /// не читает.
    pub signature: Option<[u8; 64]>,
}

/// Код отказа для отправителя образа.
///
/// Плоский и `Copy` намеренно: [`ImageSource`] не знает тип ошибки флеша, а
/// отправителю нужен код, на который он может отреагировать — исправить
/// длину, дождаться перезагрузки, сменить ключ, — а не payload. Полная
/// ошибка уходит в лог узла `domain::ota`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Rejection {
    /// Заявленная длина негодная: ноль, больше раздела, короче хвоста с
    /// версией.
    Length,
    /// Передача не сошлась с заголовком: байт больше или меньше обещанного.
    Transfer,
    /// В разделе образ, который терять нельзя (обмен ждёт сброса или ещё не
    /// подтверждён) — сначала перезагрузка.
    Busy,
    /// Версия образа не новее текущей.
    Rollback,
    /// Подписи нет, ключ не подставлен или подпись не сошлась.
    Signature,
    /// Отказ флеша или адаптера на стороне устройства.
    Device,
}
```

Трейт целиком:

```rust
/// Канал доставки образа: USB CDC, UART-протокол, сеть, SD-карта — у каждой
/// платы свой, шаблон его не выбирает. Узел `domain::ota` ждёт от него три
/// вещи: заголовок, куски и способ сообщить исход отправителю.
///
/// Порт — весь протокол обновления с точки зрения устройства, а формат
/// пакетов и проверка целостности — за реализацией: узел про них не знает.
pub trait ImageSource {
    /// Отказ канала: обрыв связи, таймаут, ошибка протокола. Узел на него
    /// выходит `TaskExit::Failed` — граф перезапустит его с backoff'ом.
    type Error;

    /// Ждёт заголовок следующего образа — сколько угодно долго.
    ///
    /// Узел зовёт это в начале каждого цикла и висит здесь всё время, пока
    /// обновления нет. Отменять этот future нельзя (`select!` с таймаутом
    /// уронил бы полупрочитанный заголовок), поэтому у узла OTA нет
    /// `watchdog:`.
    async fn begin(&mut self) -> Result<Announce, Self::Error>;

    /// Следующий кусок образа — любой длины, хоть в один байт; `None` —
    /// передача окончена.
    ///
    /// Срез заимствован у источника: буфер принадлежит каналу (DMA-буфер,
    /// пакет протокола), и копировать его ради интерфейса незачем — целые
    /// слова уходят во флеш прямо отсюда.
    async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error>;

    /// Исход цикла: `Ok(())` — образ принят и помечен к обмену на следующем
    /// сбросе, `Err(_)` — отвергнут, с кодом для отправителя.
    ///
    /// Что с этим делать — ответить хосту, сбросить МК
    /// (`cortex_m::peripheral::SCB::sys_reset()`), и то и другое — решает
    /// реализация: только она знает, когда устройство можно перезапускать.
    /// Вернулось `Ok(())` — узел ждёт следующий `begin`.
    async fn finish(&mut self, outcome: Result<(), Rejection>) -> Result<(), Self::Error>;
}
```

- [ ] **Step 2: Реэкспорт в `ports/src/lib.rs`**

`pub use download::{Announce, DownloadError, ImageSource, Rejection};`

- [ ] **Step 3: `test_support.rs` — `FakeLink` под новый порт**

Структура и `impl`:

```rust
/// Канал, отдающий заранее нарезанные куски; на `fail_at`-м вызове `next`
/// отказывает — так разыгрывается обрыв связи. Заголовки — по одному на
/// цикл; когда они кончаются, `begin` отвечает отказом — единственный выход
/// из цикла узла в тесте.
pub(crate) struct FakeLink {
    announces: VecDeque<Announce>,
    chunks: Vec<Vec<u8>>,
    /// Сколько кусков уже отдано — по нему тест видит, читался ли канал.
    pub(crate) next: usize,
    pub(crate) fail_at: Option<usize>,
    /// Что ответить на `finish`; `Err` разыгрывает обрыв при отчёте.
    pub(crate) finish: Result<(), &'static str>,
    /// Всё, что узел сообщил отправителю, в порядке вызовов.
    #[expect(dead_code, reason = "читают тесты узла OTA — задача 5")]
    pub(crate) finished: Vec<Result<(), Rejection>>,
}

impl FakeLink {
    pub(crate) fn of(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            announces: VecDeque::new(),
            chunks: chunks.into_iter().collect(),
            next: 0,
            fail_at: None,
            finish: Ok(()),
            finished: Vec::new(),
        }
    }

    /// Заголовки, которые `begin` отдаст по одному на цикл.
    #[expect(dead_code, reason = "зовут тесты узла OTA — задача 5")]
    pub(crate) fn announcing(mut self, announces: impl IntoIterator<Item = Announce>) -> Self {
        self.announces = announces.into_iter().collect();
        self
    }
}

impl ImageSource for FakeLink {
    type Error = &'static str;

    async fn begin(&mut self) -> Result<Announce, Self::Error> {
        self.announces.pop_front().ok_or("канал закрыт")
    }

    async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error> {
        if self.fail_at == Some(self.next) {
            return Err("обрыв канала");
        }
        let i = self.next;
        self.next += 1;
        Ok(self.chunks.get(i).map(Vec::as_slice))
    }

    async fn finish(&mut self, outcome: Result<(), Rejection>) -> Result<(), Self::Error> {
        self.finished.push(outcome);
        self.finish
    }
}
```

Импорты: `use std::collections::VecDeque;` и `use ports::{Announce, FirmwareUpdate, ImageSource, Rejection, SignedFirmwareUpdate, VerifyError};`.

- [ ] **Step 4: Проверить**

Run: `cargo xtask lint`
Expected: `EXIT=0`. Два `#[expect(dead_code)]` выше — ровно потому, что `announcing` и `finished` до задачи 5 никто не трогает, а clippy идёт с `--all-targets -D warnings`; `expect` вместо `allow` — чтобы в задаче 5 их нельзя было забыть (невыполненное ожидание — тоже предупреждение).

Run: `cargo xtask test host`
Expected: `EXIT=0`; тесты `download` не изменились и зелёные.

- [ ] **Step 5: Commit**

```bash
git add crates-host/domain/ports/src crates-host/domain/src/test_support.rs
git commit -m "feat(ports): ImageSource — begin/finish, Announce и код отказа Rejection

Канал становится полным протоколом с точки зрения устройства: заголовок,
куски, отчёт. Что делать с исходом — ack хосту, сброс — решает реализация
в проекте; узлу OTA остаётся позвать finish.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `domain::ota` — цикл без подписи и `run`

**Files:**
- Create: `crates-host/domain/src/ota.rs`
- Modify: `crates-host/domain/src/lib.rs` (`pub mod ota;`)

**Interfaces:**
- Consumes: `ports::{Announce, DownloadError, FirmwareUpdate, ImageSource, Rejection, UpdateError}`, `crate::download::receive`, `supervisor::runtime::TaskExit`, `defmt_or_log::{Debug2Format, info, warn}`.
- Produces: `domain::ota::Inputs<S, F> { pub link: S, pub flash: F }`; `pub async fn run<S, F>(link: &mut S, flash: &mut F) -> TaskExit`; приватные `Mode<F>`, `cycle`, `attempt`, `plain_check`, `plain_apply`, `download_rejection`, `update_rejection`.

- [ ] **Step 1: Тесты — сначала**

Создать `ota.rs` только с модулем тестов и заглушками, чтобы увидеть красный прогон:

```rust
//! Узел `OTA` графа задач — см. doc-комментарий после реализации.

#[cfg(test)]
mod tests {
    use super::{Mode, cycle, plain_apply, plain_check, run};
    use crate::test_support::{FakeFlash, FakeLink, block_on};
    use ports::{Announce, Rejection};
    use supervisor::runtime::TaskExit;

    fn plain() -> Mode<FakeFlash> {
        Mode {
            check: plain_check,
            apply: plain_apply,
        }
    }

    fn announce(len: usize) -> Announce {
        Announce {
            len: len as u32,
            signature: None,
        }
    }

    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 3) as u8).collect()
    }

    /// Счастливый путь: раздел подготовлен один раз, байты на месте, обмен
    /// запрошен, отправитель получил `Ok`.
    #[test]
    fn applies_an_unsigned_image_and_reports_success() {
        let image = image(64);
        let mut link =
            FakeLink::of(image.chunks(13).map(<[u8]>::to_vec)).announcing([announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(flash.prepared, Some(64));
        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.updated, 1, "обмен разделов запрошен один раз");
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Занятость — до стирания и до чтения канала: адаптер и так отказал бы
    /// в `prepare`, но отправитель услышал бы `Device` вместо «перезагрузи».
    #[test]
    fn reports_busy_before_touching_the_partition_or_the_link() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.busy = true;

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Busy)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    #[test]
    fn maps_a_bad_length_to_length_without_erasing() {
        let mut link = FakeLink::of([]).announcing([announce(65)]);
        let mut flash = FakeFlash::new(64, 8);

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    #[test]
    fn maps_excess_data_to_transfer() {
        let mut link = FakeLink::of([vec![0; 16], vec![0; 8]]).announcing([announce(16)]);
        let mut flash = FakeFlash::new(64, 8);

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Transfer)]);
        assert_eq!(flash.updated, 0, "обмен не запрашивался");
    }

    #[test]
    fn maps_a_flash_failure_on_apply_to_device() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_updated = Err("флеш отказал");

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
    }

    /// Обрыв канала посреди приёма: сообщать некому, цикл кончается ошибкой
    /// канала, `finish` не зовётся.
    #[test]
    fn a_link_failure_while_receiving_ends_the_cycle_without_a_report() {
        let mut link = FakeLink::of([vec![0; 8], vec![0; 8]]).announcing([announce(16)]);
        link.fail_at = Some(1);
        let mut flash = FakeFlash::new(64, 8);

        let failed = block_on(cycle(&mut link, &mut flash, &plain()));

        assert_eq!(failed, Err("обрыв канала"));
        assert!(link.finished.is_empty(), "сообщать некому");
    }

    #[test]
    fn a_link_failure_on_finish_ends_the_cycle() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        link.finish = Err("обрыв при отчёте");
        let mut flash = FakeFlash::new(64, 8);

        let failed = block_on(cycle(&mut link, &mut flash, &plain()));

        assert_eq!(failed, Err("обрыв при отчёте"));
    }

    /// `run` крутит циклы, пока жив канал, и выходит `Failed` на его отказе —
    /// здесь на исчерпании сценария: второй `begin` отвечает отказом.
    #[test]
    fn run_serves_until_the_link_fails() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        let exit = block_on(run(&mut link, &mut flash));

        assert_eq!(exit, TaskExit::Failed);
        assert_eq!(link.finished, vec![Ok(())], "первый цикл дошёл до отчёта");
    }
}
```

В `lib.rs` после `pub mod app;`:

```rust
/// Узел `OTA`: приём образа по каналу и его применение — в цикле, пока жив
/// канал. Проекту остаётся реализация `ImageSource` в `bsp` — см. модуль.
pub mod ota;
```

- [ ] **Step 2: Убедиться, что красный**

Run: `cargo xtask test host`
Expected: ошибки компиляции — `Mode`, `cycle`, `run` не найдены.

- [ ] **Step 3: Реализация**

Заменить первую строку `ota.rs` (`//! Узел … см. doc-комментарий…`) на полный модуль; `mod tests` остаётся внизу:

```rust
//! Узел `OTA` графа задач: принимает образ по каналу и применяет его — в
//! цикле, пока жив канал.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта (см.
//! `domain::app`): что узел делает — логика, и проверяется она на хосте на
//! фейках обоих портов. Проекту остаётся канал — реализация
//! [`ImageSource`] под свой транспорт (USB CDC, UART, сеть) в `bsp`:
//! заголовок, куски, отчёт отправителю — и одна строка в `fragments:` графа.
//!
//! Два входа — [`run`] и [`run_signed`] — потому что применение различается
//! по Cargo-варианту (`signed`), а Liquid в `domain` запрещён. Выбор делает
//! `crates-cross/app/src/graph.rs`: `OTA_FRAG` или `OTA_SIGNED_FRAG`. Обе
//! функции лежат в любом проекте; во flash попадает только позванная —
//! generic без вызова не мономорфизируется.
//!
//! Что узел делает с исходом, он не решает: [`ImageSource::finish`] получает
//! `Ok(())` или код отказа [`Rejection`], а ответить хосту, сбросить МК или и
//! то и другое — выбор реализации порта. Подробности — `docs/ota.md`.

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use ports::{Announce, DownloadError, FirmwareUpdate, ImageSource, Rejection, UpdateError};
use supervisor::runtime::TaskExit;

use crate::download;

/// Что узлу нужно на входе. Строит compose-site из полей `Board`:
///
/// ```ignore
/// fragments: [::domain::OTA_FRAG = ::domain::ota::Inputs { link: board.ota_link, flash: board.ota }];
/// ```
///
/// Тип объявлен здесь, а не назван графом, по правилу фрагментов
/// `supervisor`: подсистема говорит, что ей нужно, приложение это
/// конструирует — зависимость направлена от приложения к подсистеме и никогда
/// обратно.
#[derive(Debug)]
pub struct Inputs<S, F> {
    /// Канал доставки образа — реализация `ImageSource` из `bsp`.
    pub link: S,
    /// Адаптер обновления — `board.ota`.
    pub flash: F,
}

/// Узел `OTA` без проверки подписи: принятый образ применяется
/// [`FirmwareUpdate::mark_updated`].
///
/// Не возвращается, пока жив канал: после каждого цикла — удачного или нет —
/// ждёт следующий заголовок. `TaskExit::Failed` — только когда отказал сам
/// канал (`begin`, `next` или `finish`); граф перезапустит узел с backoff'ом,
/// и объект канала вернётся в слот к новому прогону. `Completed` не
/// возвращается никогда.
///
/// Без `watchdog:` намеренно: узел законно висит в `begin()` сколько угодно,
/// а опрашивать его с таймаутом нельзя — отмена future уронила бы
/// полупрочитанный заголовок. Железо кормит узел `APP`.
pub async fn run<S, F>(link: &mut S, flash: &mut F) -> TaskExit
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    info!("domain: узел OTA запущен");
    serve(
        link,
        flash,
        &Mode {
            check: plain_check,
            apply: plain_apply,
        },
    )
    .await
}

/// Чем режимы отличаются: проверка заголовка до стирания и применение.
///
/// Указатели на функции, а не трейт: две пары по нескольку строк не стоят
/// публичной абстракции, а цикл при этом один — и тестируется один.
struct Mode<F> {
    /// Проверка заголовка ДО стирания раздела.
    check: fn(&Announce) -> Result<(), Rejection>,
    /// Применение принятого образа длиной `len`.
    apply: fn(&mut F, &Announce, u32) -> Result<(), Rejection>,
}

/// Циклы до отказа канала.
async fn serve<S, F>(link: &mut S, flash: &mut F, mode: &Mode<F>) -> TaskExit
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    loop {
        if let Err(err) = cycle(link, flash, mode).await {
            warn!("ota: отказ канала: {:?}", Debug2Format(&err));
            return TaskExit::Failed;
        }
    }
}

/// Один цикл: заголовок → проверки до стирания → приём → применение → отчёт.
///
/// `Err` — отказал сам канал, и сообщать отправителю уже некому; всё
/// остальное ушло ему через `finish` кодом отказа.
async fn cycle<S, F>(link: &mut S, flash: &mut F, mode: &Mode<F>) -> Result<(), S::Error>
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    let announce = link.begin().await?;
    info!("ota: заголовок: {} байт", announce.len);
    let outcome = attempt(link, flash, &announce, mode).await?;
    match outcome {
        Ok(()) => info!("ota: образ принят, обмен разделов на следующем сбросе"),
        Err(rejection) => warn!("ota: образ отвергнут: {:?}", rejection),
    }
    link.finish(outcome).await
}

/// От заголовка до применения. Внешний `Result` — канал
/// (`DownloadError::Source`), внутренний — вердикт для отправителя.
async fn attempt<S, F>(
    link: &mut S,
    flash: &mut F,
    announce: &Announce,
    mode: &Mode<F>,
) -> Result<Result<(), Rejection>, S::Error>
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
    // отправитель услышал бы `Device` вместо «сначала перезагрузи».
    match flash.is_busy() {
        Ok(false) => {}
        Ok(true) => return Ok(Err(Rejection::Busy)),
        Err(err) => {
            warn!("ota: флеш не ответил о занятости: {:?}", Debug2Format(&err));
            return Ok(Err(Rejection::Device));
        }
    }
    if let Err(rejection) = (mode.check)(announce) {
        return Ok(Err(rejection));
    }
    let len = match download::receive(link, flash, announce.len).await {
        Ok(len) => len,
        Err(err) => {
            warn!("ota: приём не удался: {:?}", Debug2Format(&err));
            return Ok(Err(download_rejection(err)?));
        }
    };
    Ok((mode.apply)(flash, announce, len))
}

/// Без подписи заголовок проверять нечего — длину сверит `receive`.
fn plain_check(_: &Announce) -> Result<(), Rejection> {
    Ok(())
}

/// Без подписи применение — просьба об обмене разделов; проверок у неё нет
/// намеренно (см. [`FirmwareUpdate::mark_updated`]).
fn plain_apply<F>(flash: &mut F, _: &Announce, _: u32) -> Result<(), Rejection>
where
    F: FirmwareUpdate,
    F::Error: Debug,
{
    flash.mark_updated().map_err(|err| {
        warn!("ota: пометить обмен не удалось: {:?}", Debug2Format(&err));
        Rejection::Device
    })
}

/// Код отказа для отправителя; `Err` — отказал сам канал, и сообщать некому.
fn download_rejection<S, F>(err: DownloadError<S, F>) -> Result<Rejection, S> {
    Ok(match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. } | DownloadError::Incomplete { .. } => {
            Rejection::Transfer
        }
        DownloadError::UnusableGranularity(_) | DownloadError::Flash(_) => Rejection::Device,
        DownloadError::Source(err) => return Err(err),
    })
}

/// Код отказа для отправителя по ошибке применения с подписью.
fn update_rejection<E>(err: &UpdateError<E>) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        UpdateError::NoPublicKey | UpdateError::BadSignature => Rejection::Signature,
        UpdateError::Flash(_) => Rejection::Device,
    }
}
```

`update_rejection` пока никто не зовёт — это задача 6. Чтобы `-D warnings` не упал на `dead_code`, на этом шаге поставить над ней `#[expect(dead_code, reason = "применение с подписью — задача 6")]` и снять в задаче 6.

Одновременно **снять три `#[expect(dead_code, …)]` из `test_support.rs`** (поле `FakeFlash::updated`, поле `FakeLink::finished`, метод `FakeLink::announcing`): тесты этой задачи их используют, и невыполненное ожидание само стало бы предупреждением.

- [ ] **Step 4: Прогнать**

Run: `cargo xtask lint`
Expected: `EXIT=0`. Возможная придирка clippy `needless_pass_by_value` на `download_rejection(err)` — не относится: `Source(err)` из него вынимается по значению.

Run: `cargo xtask test host`
Expected: `EXIT=0`; восемь тестов `ota::tests::*` зелёные.

- [ ] **Step 5: Commit**

```bash
git add crates-host/domain/src/ota.rs crates-host/domain/src/lib.rs
git commit -m "feat(domain): узел OTA — цикл begin → receive → mark_updated → finish

Логика узла живёт в domain и проверяется на хосте: занятость раздела и
длина отказывают до стирания, отказы уходят отправителю кодом Rejection,
и только отказ самого канала кончает задачу Failed — граф её перезапустит.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: `domain::ota` — режим с подписью и `run_signed`

**Files:**
- Modify: `crates-host/domain/src/ota.rs`

**Interfaces:**
- Consumes: `crate::update::apply_signed`, `ports::SignedFirmwareUpdate`, `update_rejection` из задачи 5.
- Produces: `pub async fn run_signed<S, F>(link: &mut S, flash: &mut F) -> TaskExit`; приватные `signed_check`, `signed_apply`.

- [ ] **Step 1: Тесты**

Добавить в `mod tests`:

```rust
    // — в импортах тестов: —
    // use super::{Mode, cycle, plain_apply, plain_check, run, run_signed, signed_apply, signed_check};
    // use crate::firmware::pack;
    // use crate::test_support::{FakeFlash, FakeLink, SIGNATURE, block_on};
    // use crate::update::VERSION_BYTES;
    // use ports::{Announce, Rejection, VerifyError};

    fn signed() -> Mode<FakeFlash> {
        Mode {
            check: signed_check,
            apply: signed_apply,
        }
    }

    fn signed_announce(len: usize) -> Announce {
        Announce {
            len: len as u32,
            signature: Some(SIGNATURE),
        }
    }

    /// Образ с версией в хвосте — как его собирает `cargo xtask build`.
    fn versioned_image(len: usize, version: u32) -> Vec<u8> {
        let mut image = image(len);
        image[len - VERSION_BYTES as usize..].copy_from_slice(&version.to_le_bytes());
        image
    }

    /// Счастливый путь с подписью: подпись из заголовка и длина доехали до
    /// адаптера, `mark_updated` не звался — обмен делает только проверка.
    #[test]
    fn applies_a_signed_image_with_the_signature_from_the_announce() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image.clone()]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        block_on(cycle(&mut link, &mut flash, &signed())).expect("канал жив");

        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.verified, vec![(SIGNATURE, 64)]);
        assert_eq!(flash.updated, 0);
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Без подписи в заголовке — отказ ДО стирания: стирание уничтожило бы
    /// образ, в который устройство откатывается, а применить всё равно нечем.
    #[test]
    fn refuses_a_signed_update_without_a_signature_before_erasing() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        block_on(cycle(&mut link, &mut flash, &signed())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    /// Та же версия — откат, и код для отправителя свой.
    #[test]
    fn maps_a_rollback_to_rollback() {
        let image = versioned_image(64, pack(1, 2, 3));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        block_on(cycle(&mut link, &mut flash, &signed())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Rollback)]);
        assert!(flash.verified.is_empty(), "до криптографии дойти не должно");
    }

    #[test]
    fn maps_a_bad_signature_to_signature() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);
        flash.verify = Err(VerifyError::BadSignature);

        block_on(cycle(&mut link, &mut flash, &signed())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
    }

    #[test]
    fn maps_a_zero_key_to_signature() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);
        flash.key = [0; 32];

        block_on(cycle(&mut link, &mut flash, &signed())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
        assert!(flash.verified.is_empty());
    }

    #[test]
    fn run_signed_serves_until_the_link_fails() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        let exit = block_on(run_signed(&mut link, &mut flash));

        assert_eq!(exit, TaskExit::Failed);
        assert_eq!(link.finished, vec![Ok(())]);
    }
```

- [ ] **Step 2: Красный прогон**

Run: `cargo xtask test host`
Expected: ошибки компиляции — `run_signed`, `signed_check`, `signed_apply` не найдены.

- [ ] **Step 3: Реализация**

После `run` добавить:

```rust
/// Узел `OTA` с проверкой подписи: принятый образ применяется
/// [`update::apply_signed`](crate::update::apply_signed) — длина, занятость,
/// версия, ключ, подпись, в этом порядке.
///
/// Поведение по каналу — как у [`run`]. Отличие до приёма одно: заголовок
/// без подписи отвергается ДО стирания раздела — применить такой образ всё
/// равно нечем, а стирание уничтожило бы образ, в который устройство
/// откатывается.
pub async fn run_signed<S, F>(link: &mut S, flash: &mut F) -> TaskExit
where
    S: ImageSource,
    S::Error: Debug,
    F: SignedFirmwareUpdate,
    F::Error: Debug,
{
    info!("domain: узел OTA (с подписью) запущен");
    serve(
        link,
        flash,
        &Mode {
            check: signed_check,
            apply: signed_apply,
        },
    )
    .await
}
```

После `plain_apply`:

```rust
/// С подписью заголовок обязан её нести — иначе отказ до стирания.
fn signed_check(announce: &Announce) -> Result<(), Rejection> {
    if announce.signature.is_none() {
        return Err(Rejection::Signature);
    }
    Ok(())
}

/// С подписью применение — [`update::apply_signed`](crate::update::apply_signed)
/// с подписью из заголовка.
fn signed_apply<F>(flash: &mut F, announce: &Announce, len: u32) -> Result<(), Rejection>
where
    F: SignedFirmwareUpdate,
    F::Error: Debug,
{
    // `signed_check` уже отказал бы без подписи; ветка `None` здесь — чтобы
    // не паниковать, а не потому что она достижима.
    let Some(signature) = announce.signature.as_ref() else {
        return Err(Rejection::Signature);
    };
    update::apply_signed(flash, signature, len).map_err(|err| {
        warn!("ota: применение отвергнуто: {:?}", Debug2Format(&err));
        update_rejection(&err)
    })
}
```

Импорты: `use ports::{Announce, DownloadError, FirmwareUpdate, ImageSource, Rejection, SignedFirmwareUpdate, UpdateError};` и `use crate::{download, update};`. Снять `#[expect(dead_code, …)]` с `update_rejection`.

- [ ] **Step 4: Прогнать**

Run: `cargo xtask lint`
Expected: `EXIT=0`.

Run: `cargo xtask test host`
Expected: `EXIT=0`; четырнадцать тестов `ota::tests::*` зелёные.

- [ ] **Step 5: Commit**

```bash
git add crates-host/domain/src/ota.rs
git commit -m "feat(domain): узел OTA с подписью — run_signed поверх apply_signed

Отсутствие подписи в заголовке отвергается до стирания раздела; отказы
apply_signed (откат, ключ, подпись, занятость) уходят отправителю своим
кодом. Цикл тот же, что без подписи, — различаются две функции режима.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Фрагменты `OTA_FRAG`/`OTA_SIGNED_FRAG` и `supervisor` с `boot:`-привязкой

**Files:**
- Modify: `Cargo.lock` (корневой)
- Modify: `crates-host/domain/src/ota.rs` (фрагменты в конце файла, перед `mod tests`)

**Interfaces:**
- Consumes: `supervisor_fragment!` с элементом `boot: <name>: <Type>;` (rust-lib `647505b` и новее).
- Produces: `::domain::OTA_FRAG`, `::domain::OTA_SIGNED_FRAG`; оба требуют на compose-site имена `OtaLink`, `OtaFlash`, `RestartPolicy`, `backoff()`.

- [ ] **Step 1: Поднять `supervisor` в корневом lock**

Run: `cargo update -p supervisor`
Expected: в `Cargo.lock` у всех пакетов из `git+https://github.com/Sergycb/rust-lib.git` новый rev (`647505b…` или новее); других изменений в lock нет (все 14 коммитов между `7174234` и `647505b` — фича `boot:`-привязки фрагмента).

Run: `git diff --stat Cargo.lock` — только `Cargo.lock`.

- [ ] **Step 2: Убедиться, что старый rev фичу не знал (по желанию, для протокола)**

До `cargo update` фрагмент с `boot:` из шага 3 должен был бы падать с диагностикой «`boot:` … compose site». Проверять не обязательно; достаточно, что после обновления он компилируется.

- [ ] **Step 3: Фрагменты в `ota.rs`**

Перед `#[cfg(test)] mod tests`:

```rust
// Узлы объявлены здесь же, где лежит их задача, — как `APP_FRAG` в
// `domain::app`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент и кормит его входом. Два фрагмента, а не один,
// потому что применение различается по Cargo-варианту `signed`, а Liquid в
// `domain` запрещён: какой из двух назвать — решает `graph.rs`.
//
// `boot: inputs: …` — собственная привязка фрагмента: compose-site пишет
// `fragments: [::domain::OTA_FRAG = ::domain::ota::Inputs { link: …, flash: … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализаторы слотов
// ниже читают её поля. Так фрагмент не видит `Board` вовсе — только то, что
// ему дали.
//
// Имена `OtaLink`, `OtaFlash`, `RestartPolicy`, `backoff()` резолвятся НЕ
// здесь, а на compose-site (`macro_rules!` подставляет токены в место
// вызова). Для политики это прецедент `APP_FRAG`; для типов слотов — его
// расширение: тип ресурсного слота — `static`, назвать его фрагмент обязан,
// а конкретный тип (`bsp::ota::Ota`, транспорт проекта) `domain` не видит и
// видеть не должен. `graph.rs` объявляет оба псевдонима одной строкой каждый.
//
// Без `watchdog:` намеренно — см. `run`.
supervisor::supervisor_fragment! {
    name: OTA_FRAG;
    boot: inputs: $crate::ota::Inputs<OtaLink, OtaFlash>;

    node OTA, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
        resources: [LINK: OtaLink = inputs.link, FLASH: OtaFlash = inputs.flash],
        task: $crate::ota::run(ctx.link, ctx.flash);
}

supervisor::supervisor_fragment! {
    name: OTA_SIGNED_FRAG;
    boot: inputs: $crate::ota::Inputs<OtaLink, OtaFlash>;

    node OTA, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
        resources: [LINK: OtaLink = inputs.link, FLASH: OtaFlash = inputs.flash],
        task: $crate::ota::run_signed(ctx.link, ctx.flash);
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo xtask lint`
Expected: `EXIT=0` — проц-макрос разбирает грамматику фрагмента при сборке `domain` (`boot:` первым после `name:`, узел с полями); семантика (типы, `inputs.link`) проверяется только на compose-site, в задаче 8.

Run: `cargo xtask test host`
Expected: `EXIT=0`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates-host/domain/src/ota.rs
git commit -m "feat(domain): фрагменты OTA_FRAG/OTA_SIGNED_FRAG с собственной boot:-привязкой

supervisor поднят до rust-lib 647505b: фрагмент объявляет boot: inputs:
Inputs<OtaLink, OtaFlash>, compose-site кормит его полями Board. Имена
типов слотов резолвятся на compose-site — расширение прецедента
APP_WATCHDOG с констант на типы.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: `crates-cross` — заглушка канала, поле `Board`, compose-site

**Files:**
- Modify: `crates-cross/bsp/src/ota.rs` (заглушка `Link`)
- Modify: `crates-cross/bsp/src/board.rs` (поле `ota_link`)
- Modify: `crates-cross/app/src/graph.rs` (`OtaLink`/`OtaFlash`, `fragments:`)
- Modify: `crates-cross/app/src/main.rs` (комментарий, строки 105–112)
- Modify: `docs/modules/bsp-ota.md` (`include_str!` в `bsp/src/ota.rs`)
- Modify: `docs/modules/app-graph.md` (раздел «OTA: что делает шаблон и что остаётся вам», строки 277–296)

**Interfaces:**
- Consumes: `::domain::OTA_FRAG`, `::domain::OTA_SIGNED_FRAG`, `::domain::ota::Inputs`, `ports::{Announce, ImageSource, Rejection}`.
- Produces: `bsp::ota::Link` (реализует `ImageSource`, `Error = Infallible`), `bsp::Board::ota_link: Link`; в `graph.rs` — `type OtaLink = bsp::ota::Link; type OtaFlash = bsp::ota::Ota;`.

- [ ] **Step 1: `bsp/src/ota.rs` — заглушка**

Добавить после блока `{%- endif %}` с псевдонимом `Ota` (строка 63) и перед `pub(crate) fn new`:

```rust

/// Канал доставки образа этой платы — заглушка, которую проект заменяет
/// своим транспортом.
///
/// Каркас, как `domain::app::run`: узел `OTA` уже собран и спавнится графом,
/// а как приходит образ — USB CDC, UART, сеть, SD-карта — шаблон не знает.
/// Заглушка ждёт заголовок вечно, то есть узел висит в `begin()` и ничего не
/// принимает; о себе она сообщает одной строкой в лог при старте.
///
/// Заменить — значит реализовать три метода `ports::ImageSource` на объекте,
/// собранном в `Board::new` из вашей периферии: `begin` — дождаться и
/// разобрать заголовок (длина{% if signed == "true" %}, подпись{% endif %}),
/// `next` — отдавать куски образа, `finish` — сообщить исход отправителю и
/// решить, перезапускать ли МК (`cortex_m::peripheral::SCB::sys_reset()`).
/// Формат пакетов и проверка целостности — ваши; узел про них не знает.
/// Всё, что не зависит от канала — сверка длины до стирания, буферизация до
/// слова флеша, порядок проверок подписи, — уже в `domain::ota`.
pub struct Link;

impl ImageSource for Link {
    type Error = Infallible;

    async fn begin(&mut self) -> Result<Announce, Self::Error> {
        defmt::warn!("bsp: транспорт OTA не реализован — узел OTA ждёт впустую");
        core::future::pending().await
    }

    async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error> {
        Ok(None)
    }

    async fn finish(&mut self, _outcome: Result<(), Rejection>) -> Result<(), Self::Error> {
        Ok(())
    }
}
```

Импорты: `use core::convert::Infallible;` — отдельной группой перед `use embassy_boot::…`, а `use ports::{Announce, ImageSource, Rejection};` — в ту же группу, что `embassy_*`, после `use embassy_sync::…` (rustfmt сортирует внутри группы, и `lint cross` в сгенерированном проекте это проверит). `ports` уже зависимость `bsp`; `defmt` тоже.

- [ ] **Step 2: `bsp/src/board.rs` — поле**

После поля `pub ota: crate::ota::Ota,` (внутри `{%- if ota == "true" %}`):

```rust
    /// Канал доставки образа — заглушка `bsp::ota::Link`, пока проект не
    /// подставит свой транспорт. Вместе с `ota` уезжает в узел `OTA`
    /// графа (`fragments:` в `crates-cross/app/src/graph.rs`).
    pub ota_link: crate::ota::Link,
```

В `Board::new`, после `ota: crate::ota::new(flash),`:

```rust
            ota_link: crate::ota::Link,
```

Строка 52 doc-комментария у `ota` («Канал доставки — за пределами шаблона, см. `docs/modules/bsp-ota.md`») → «Канал доставки — поле `ota_link` ниже.».

- [ ] **Step 3: `app/src/graph.rs` — псевдонимы и `fragments:`**

После `const APP_WATCHDOG: Duration = …;` и перед `supervisor_graph! {`:

```rust
{%- if ota == "true" %}

// Два имени, которые называет фрагмент узла OTA (`domain::ota`): тип
// ресурсного слота — `static`, назвать его фрагмент обязан, а конкретные
// типы знает только эта сторона. Подставили свой транспорт вместо заглушки
// — меняйте правую часть первой строки.
type OtaLink = bsp::ota::Link;
type OtaFlash = bsp::ota::Ota;
{%- endif %}
```

Строку `fragments: [::domain::APP_FRAG];` заменить на:

```rust
    fragments: [::domain::APP_FRAG{% if ota == "true" %}, ::domain::OTA{% if signed == "true" %}_SIGNED{% endif %}_FRAG = ::domain::ota::Inputs { link: board.ota_link, flash: board.ota }{% endif %}];
```

Комментарий над `fragments:` дополнить абзацем:

```rust
    // Фрагмент со своей `boot:`-привязкой кормится прямо здесь (`= …`):
    // выражение вычисляется в прологе `spawn_all` первой строкой, полями
    // `board` по частичному move — `ota_link` и `ota` уезжают в слоты узла,
    // `watchdog` строкой ниже забирает сторож, остаток `board` дропается в
    // конце пролога.
```

- [ ] **Step 4: `app/src/main.rs` — комментарий про заборы**

Строки 108–110 (`// Сейчас такой забор один (сторож в блоке `watchdog:`), и здесь же / // окажутся ваши: …`) заменить на:

```rust
    // Такие заборы уже есть — сторож в блоке `watchdog:`{% if ota == "true" %}, канал и адаптер
    // OTA в `fragments:` (`board.ota_link`, `board.ota`){% endif %} — и здесь же
    // окажутся ваши: `resources: [SLOT: T = board.<поле>]` вместо
```

(строка `// провайд_<slot>(..) перед этой строкой…` и далее — без изменений).

- [ ] **Step 5: `docs/modules/bsp-ota.md` — транспорт**

Заменить всё от абзаца «Транспорт остаётся снаружи…» до конца файла на:

```markdown
Транспорт — поле `Board::ota_link`: в шаблоне это заглушка [`Link`], которая
ждёт заголовок вечно. Заменить её — значит реализовать три метода
`ports::ImageSource` на объекте, собранном из вашей периферии:

```ignore
impl ImageSource for Link {
    type Error = LinkError;
    // Дождаться и разобрать заголовок: длина{% if signed == "true" %} и подпись{% endif %}.
    async fn begin(&mut self) -> Result<Announce, LinkError> { .. }
    // Куски образа любой длины; `None` — конец.
    async fn next(&mut self) -> Result<Option<&[u8]>, LinkError> { .. }
    // Исход — отправителю; здесь же решается, перезапускать ли МК.
    async fn finish(&mut self, outcome: Result<(), Rejection>) -> Result<(), LinkError> {
        self.send_ack(outcome).await?;
        if outcome.is_ok() {
            cortex_m::peripheral::SCB::sys_reset();
        }
        Ok(())
    }
}
```

Всё остальное — сверка длины до стирания, `prepare` один раз, буферизация до
слова флеша, порядок проверок подписи, коды отказа — уже в узле
`domain::ota`, который граф спавнит из `fragments:`
(`crates-cross/app/src/graph.rs`). Без графа зовите узел сами:
`domain::ota::run(&mut board.ota_link, &mut board.ota).await`.

Подтверждение образа (`mark_booted`) и то, почему без него обновление живёт
один запуск, описано в `adapters::ota`; вызов стоит в `main` и его стоит
перенести туда, где устройство доказало работоспособность.
```

- [ ] **Step 6: `docs/modules/app-graph.md` — раздел OTA**

Заменить раздел `# OTA: что делает шаблон и что остаётся вам` (строки 277–296, до `{%- endif %}` включительно) на:

```markdown
# OTA: узел уже в графе, вам остаётся канал

Узел `OTA` объявлен в `domain::ota` (`OTA_FRAG` без подписи,
`OTA_SIGNED_FRAG` с ней — какой из двух, решила генерация) и спавнится
отсюда строкой в `fragments:`. Он в цикле ждёт заголовок, принимает образ
(`domain::download::receive` — сверка длины до стирания, буферизация до
слова флеша), применяет его (`mark_updated` или `domain::update::apply_signed`)
и сообщает исход отправителю кодом `ports::Rejection`. Всё это — host-тесты
в `crates-host/domain/src/ota.rs`.

Вам остаётся канал — то, чего шаблон знать не может: USB CDC, UART, сеть,
SD-карта, у каждого свой формат пакета и своя проверка целостности. Это
реализация `ports::ImageSource` на месте заглушки `bsp::ota::Link`
(`docs/modules/bsp-ota.md`): три метода — `begin`, `next`, `finish`. Что
делать с исходом — ответить хосту, сбросить МК, и то и другое — решает
ваш `finish`; узел этого не знает.

Вход узла собирается здесь же, в `fragments:`, из полей `Board`
(`::domain::ota::Inputs { link: board.ota_link, flash: board.ota }`);
подставили свой транспорт — поправьте псевдоним `OtaLink` выше по файлу.

У узла нет `watchdog:`, и это не забывчивость: он законно висит в `begin()`
сколько угодно, а опрашивать его с таймаутом нельзя — отмена future
уронила бы полупрочитанный заголовок. Железо кормит узел `APP`.

{%- endif %}
```

- [ ] **Step 7: Проверить генерацией — вариант по умолчанию**

Из корня репозитория (пути под Git Bash; `$SCRATCH` — каталог scratchpad сессии):

```bash
SCRATCH="$HOME/AppData/Local/Temp/claude/d--Projects-rust-embedded-template/a4a3289a-9d05-449c-9389-c6b0e5c1538e/scratchpad"
mkdir -p "$SCRATCH/gen"
cargo generate --path . --name ota-default --define chip_feature=stm32f407ve --define ci=github --silent --destination "$SCRATCH/gen"
cd "$SCRATCH/gen/ota-default"
CARGO_TARGET_DIR="$SCRATCH/gen/target-default" cargo xtask lint cross
CARGO_TARGET_DIR="$SCRATCH/gen/target-default" cargo xtask build
```

Expected: оба `EXIT=0`. Первая сборка тянет `supervisor` из git и создаёт `crates-cross/Cargo.lock` — это нормально. Ошибки, которые могут вылезти именно здесь, и что они значат:

- `cannot find type OtaLink` — псевдонимы в `graph.rs` не попали под нужный `{% if %}`.
- `fragment … declares boot: but is not fed` / `is fed but declares no boot:` — расхождение между `fragments:` и фрагментом.
- `use of moved value: board` — порядок частичных move; `ota_link`/`ota` и `watchdog` — разные поля, такого быть не должно.
- `expected &mut Link, found …` — проекция `ctx.link` не совпала с сигнатурой `run` (в фикстуре `supervisor` `ctx.<slot>` — `&mut T`).

Сгенерированный проект после проверки удалить не обязательно, но в репозиторий он не попадает (лежит в scratchpad).

- [ ] **Step 8: Commit**

```bash
cd "<корень репозитория>"
git add crates-cross/bsp/src/ota.rs crates-cross/bsp/src/board.rs crates-cross/app/src/graph.rs crates-cross/app/src/main.rs docs/modules/bsp-ota.md docs/modules/app-graph.md
git commit -m "feat(cross): узел OTA в графе — заглушка канала в bsp, вход из полей Board

bsp::ota::Link ждёт заголовок вечно и заменяется транспортом проекта;
Board.ota_link и Board.ota уезжают в OTA_FRAG/OTA_SIGNED_FRAG через
собственную boot:-привязку фрагмента. Заглушка нужна, чтобы template-check
раскрывал фрагменты в обоих вариантах, а не оставлял их непроверенными.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Документация — `architecture.md`, `ota.md`, `README.md`, `AGENTS.md`, `docs/README.md`

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/ota.md`
- Modify: `README.md` (раздел «Обновление прошивки», строки 196–238; «Подпись образа», 257–263)
- Modify: `AGENTS.md`
- Modify: `docs/README.md`

- [ ] **Step 1: `docs/architecture.md`**

1. Абзац «**Узлы графа и их тела — тоже `domain`.**»: `(domain::app::APP_FRAG — узел APP шаблона)` → `(domain::app::APP_FRAG — узел APP шаблона; domain::ota::{OTA_FRAG, OTA_SIGNED_FRAG} — узел OTA, видны как ::domain::OTA_FRAG)`.
2. Абзац «**`domain` — только функции; типы — в `ports`.**»: после `тело узла APP (app::run)` добавить `и узла OTA (ota::run/run_signed — цикл над обоими портами)`.
3. Абзац «Что при этом НЕ уезжает в `domain` — цифры…» дополнить в конце:

   ```markdown
   Тем же путём приходят и **типы ресурсных слотов** узла OTA: фрагмент
   называет `OtaLink`/`OtaFlash`, а объявляет их `graph.rs`
   (`type OtaLink = bsp::ota::Link; type OtaFlash = bsp::ota::Ota;`). Слот —
   `static`, тип ему нужен, а конкретный тип знает только `cross`. Это
   расширение прецедента с констант на типы, и другого способа у фрагмента
   нет: generic-`static` не бывает.
   ```
4. В разделе «`Board` отдаёт объекты, а не периферию» после абзаца про `watchdog` (заканчивается «…(`docs/watchdog.md`).») добавить:

   ```markdown
   Второе поле, которое уезжает в граф целиком, — `ota_link`: канал доставки
   образа, в шаблоне заглушка `bsp::ota::Link` (ждёт заголовок вечно). Оно
   здесь по общему правилу — объект, реализующий порт (`ImageSource`), — и
   ради проверяемости: с ним `template-check` раскрывает фрагменты узла OTA в
   обоих вариантах, без него они не компилировались бы нигде. Проект заменяет
   заглушку своим транспортом, не трогая ни узел, ни граф
   (`docs/modules/bsp-ota.md`).
   ```

- [ ] **Step 2: `docs/ota.md`**

Строки 5–12 (абзац «**Приложение зовёт не порт напрямую…**») заменить на:

```markdown
**Приём и применение крутит узел `OTA` (`domain::ota`), приложение не зовёт
ни порт, ни `receive` само.** Узел ждёт заголовок (`ImageSource::begin`),
отказывает до стирания, если раздел занят или заголовок негодный, принимает
образ `domain::download::receive` — функцией над двумя портами, `ImageSource`
(ваш канал) и `FirmwareUpdate` (адаптер OTA), — применяет его и сообщает исход
отправителю (`ImageSource::finish`, код `ports::Rejection`). `receive` делает
всё, что одинаково для любого канала: сверяет длину с `capacity()` ДО
стирания, зовёт `prepare` один раз, копит куски до слова флеша
(`write_granularity()`), ведёт смещение и не даёт образу вылезти за обещанную
длину. Заменять это ручным циклом `prepare`/`write` в проекте не надо — там
четыре независимые грабли, каждая из которых даёт молчаливо испорченный
образ, и все они закрыты host-тестами. Что делать с исходом — ack хосту,
сброс МК — решает ваша реализация `finish`; узел ничего не сбрасывает.
```

В разделе «Подпись OTA-образа» (строки 79–84) после `применение обновления делает domain::update::apply_signed (без подписи — mark_updated() через порт)` добавить: `— из узла OTA (run_signed против run); неверная подпись приходит от адаптера своим вариантом (ports::VerifyError::BadSignature → UpdateError::BadSignature → код Signature отправителю), а не как отказ флеша,`.

- [ ] **Step 3: `README.md` — раздел «Обновление прошивки»**

Строки 196–238 (от «Всё, что не зависит от канала доставки, готово…» до «…и шаблон их не выбирает.») заменить на:

```markdown
Всё, что не зависит от канала доставки, готово и уже крутится в графе задач:
узел `OTA` (`domain::ota`) ждёт заголовок образа, принимает его в раздел `DFU`
(`domain::download::receive`), применяет (`mark_updated` или, с подписью,
`domain::update::apply_signed`) и сообщает исход отправителю. Адаптер
`adapters::ota` (поверх `embassy-boot`) пишет образ и хранит состояние
bootloader'а, `crates-cross/bsp/src/ota.rs` собирает его из линкерных символов
`memory.x`, `Board` отдаёт его полем `ota`, а граф забирает вместе с каналом
одной строкой (`fragments:` в `crates-cross/app/src/graph.rs`).

Канал остаётся за вами — USB CDC, UART, сеть, SD-карта: формат пакета и
проверка целостности у каждого свои, и шаблон их не выбирает. В `Board` он
лежит полем `ota_link`, в шаблоне это заглушка `bsp::ota::Link`, которая ждёт
заголовок вечно. Заменить её — значит реализовать три метода
`ports::ImageSource`:

```rust
impl ImageSource for Link {
    type Error = LinkError;
    // Дождаться заголовка: длина образа и, с подписью, сама подпись.
    async fn begin(&mut self) -> Result<Announce, LinkError> { /* ваш протокол */ }
    // Куски образа любой длины; None — конец передачи.
    async fn next(&mut self) -> Result<Option<&[u8]>, LinkError> { /* ваш протокол */ }
    // Исход — отправителю; здесь же решается, перезапускать ли МК.
    async fn finish(&mut self, outcome: Result<(), Rejection>) -> Result<(), LinkError> {
        self.send_ack(outcome).await?;
        if outcome.is_ok() { cortex_m::peripheral::SCB::sys_reset(); }
        Ok(())
    }
}
```

Отказы приходят отправителю кодом `Rejection` — `Length`, `Transfer`, `Busy`,
`Rollback`, `Signature`, `Device`, — а полная причина уходит в лог устройства.
Занятый раздел и негодная длина отвергаются **до** стирания: стирание
уничтожает образ, в который устройство откатывается. Писать этот цикл руками
не стоит: половина здешних граблей не видна, пока в них не попадёшь. Флеш
принимает запись только словами (от четырёх до тридцати двух байт в зависимости
от чипа), а канал отдаёт пакеты какой угодно длины — значит куски надо копить;
последний почти никогда не кратен слову; смещение надо вести самому и не дать
образу вылезти за обещанную длину. Узел и `receive` делают всё это и проверены
host-тестами на кусках по одному байту, обрыве связи, лишних данных и каждом
классе отказа — `crates-host/domain/src/ota.rs` и `download.rs`.

Под приёмом лежит `prepare`, и он не формальность: без него запись ляжет в
нестёртую память — на F2/F4/F7 молча, побитовым И со старым содержимым, на
остальных семействах ошибкой. Он же самый долгий вызов: стирание занимает от
сотен миллисекунд на чипах с мелкими страницами до нескольких секунд там, где
сектор — четверть мегабайта. Всё это время исполнитель embassy не работает
(драйвер флеша стирает внутри критической секции); поэтому у узла `OTA` нет
`watchdog:`, а кормит сторож узел `APP`. Длина нужна именно для того, чтобы
стирать не весь раздел, а столько страниц, сколько накроет образ.
```

Строки 257–263 («Подпись образа», первый абзац): `С ним вместо board.ota.mark_updated() вызывается domain::update::apply_signed(&mut board.ota, &signature, length)` → `С ним граф спавнит узел OTA с подписью (OTA_SIGNED_FRAG → domain::ota::run_signed): вместо mark_updated() принятый образ применяет domain::update::apply_signed с подписью из заголовка (Announce::signature)`; дальше абзац без изменений.

- [ ] **Step 4: `AGENTS.md`**

В абзаце раздела «Граница `domain`/`cross`»: `подсистема объявляет свои supervisor_fragment!-ом рядом со своей задачей (domain::app::APP_FRAG)` → `подсистема объявляет свои supervisor_fragment!-ом рядом со своей задачей (domain::app::APP_FRAG, domain::ota::OTA_FRAG)`.

В список «Инварианты, которые не ловит компилятор» добавить после пункта про `prepare(len)`:

```markdown
- `lib.rs` только объявляет: атрибуты, `//!`, `mod`, `pub use` — определения
  живут в модулях — `docs/conventions.md`.
- Узел OTA не сбрасывает МК и не решает, что делать с исходом: это
  `ImageSource::finish` в `bsp`; отказы до стирания (`Busy`, нет подписи,
  длина) обязаны оставаться до `receive` — `docs/ota.md`.
```

- [ ] **Step 5: `docs/README.md`**

Строка таблицы `ota.md`: «трогаете `adapters::ota`, `bsp::ota`, `domain::download` или `domain::update`» → «трогаете `adapters::ota`, `bsp::ota`, `domain::ota`, `domain::download` или `domain::update`». Строка `conventions.md`: «feature-флаги, правила добавления зависимостей, осознанные ограничения» → «feature-флаги, правила добавления зависимостей, правило `lib.rs`, осознанные ограничения».

- [ ] **Step 6: Проверить, что Liquid в документации рендерится**

Run: `cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- --quick`
Expected: `EXIT=0` — генерация всех кейсов и проверка плейсхолдеров проходят (секунды).

Run: `cargo xtask lint` и `cargo xtask test host` — по-прежнему `EXIT=0` (документация кода не трогает, но `README`/`AGENTS` — нет; это контроль, что ничего не задето случайно).

- [ ] **Step 7: Commit**

```bash
git add docs/architecture.md docs/ota.md README.md AGENTS.md docs/README.md
git commit -m "docs: поток OTA через узел domain::ota, типы слотов на compose-site, правило lib.rs

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Проверка остальных вариантов генерации

**Files:** без правок (если проверка не найдёт ошибку).

- [ ] **Step 1: Вариант с подписью**

```bash
SCRATCH="$HOME/AppData/Local/Temp/claude/d--Projects-rust-embedded-template/a4a3289a-9d05-449c-9389-c6b0e5c1538e/scratchpad"
cargo generate --path . --name ota-signed --define chip_feature=stm32f407ve --define ci=github --define signed=yes --silent --destination "$SCRATCH/gen"
cd "$SCRATCH/gen/ota-signed"
CARGO_TARGET_DIR="$SCRATCH/gen/target-signed" cargo xtask lint cross
CARGO_TARGET_DIR="$SCRATCH/gen/target-signed" cargo xtask build
```

Expected: `EXIT=0`; в `crates-cross/app/src/graph.rs` сгенерированного проекта — `::domain::OTA_SIGNED_FRAG = …`. `cargo xtask build` здесь создаёт ключевую пару и подписывает образ — это штатно.

- [ ] **Step 2: Вариант без OTA**

```bash
cargo generate --path . --name ota-none --define chip_feature=stm32f407ve --define ci=github --define ota=no --silent --destination "$SCRATCH/gen"
cd "$SCRATCH/gen/ota-none"
CARGO_TARGET_DIR="$SCRATCH/gen/target-none" cargo xtask lint cross
CARGO_TARGET_DIR="$SCRATCH/gen/target-none" cargo xtask build
```

Expected: `EXIT=0`; в `graph.rs` только `fragments: [::domain::APP_FRAG];`, псевдонимов `OtaLink`/`OtaFlash` нет, `bsp/src/ota.rs` отсутствует.

- [ ] **Step 3: Вариант без графа**

```bash
cargo generate --path . --name ota-nograph --define chip_feature=stm32f407ve --define ci=github --define graph=no --silent --destination "$SCRATCH/gen"
cd "$SCRATCH/gen/ota-nograph"
CARGO_TARGET_DIR="$SCRATCH/gen/target-nograph" cargo xtask lint cross
CARGO_TARGET_DIR="$SCRATCH/gen/target-nograph" cargo xtask build
```

Expected: `EXIT=0`; `Board` содержит `ota_link`, `graph.rs` пуст (нет `supervisor_graph!`), clippy не ругается на неиспользуемую заглушку (`Link` — `pub`, публичные типы `dead_code` не дают).

- [ ] **Step 4: Итоговый прогон в репозитории шаблона**

Из корня, по одной команде (не `precommit`, чтобы видеть, какая упала):

```bash
cargo xtask lint
cargo xtask test host
```

Expected: оба `EXIT=0`.

- [ ] **Step 5: Если что-то упало**

Исправить в репозитории шаблона (не в сгенерированной копии), закоммитить `fix(...)` с причиной, перегенерировать упавший вариант в новый каталог (`--name ota-signed-2` и т.п. — `cargo generate` в существующий не пишет) и повторить его проверку.

---

## Self-review (выполнен при написании плана)

**Покрытие спеки:**
- Часть A: правило и три модуля `ports` — задача 1; `test_support` + один фейк на порт — задача 2; `bsp/lib.rs` без изменений — так и есть. ✔
- `Announce`, `Rejection`, `begin`/`finish` — задача 4. ✔
- `VerifyError`, `UpdateError::BadSignature`, маппинг в адаптере, тест — задача 3. ✔
- `domain::ota`: `Inputs`, `run`, `run_signed`, один цикл, порядок проверок (`Busy` → подпись → длина внутри `receive`), таблица кодов, без `watchdog:` — задачи 5–6; фрагменты — задача 7. ✔
- `cargo update -p supervisor` — задача 7. ✔
- Заглушка `Link`, `Board.ota_link`, псевдонимы и `fragments:` в `graph.rs`, `main.rs`, `graph=no` в документации — задача 8. ✔
- Тесты из спеки: все девять сценариев — задачи 5–6 (плюс `NoPublicKey` → `Signature`); адаптер — задача 3. ✔
- Документация: все шесть файлов — задачи 8–9. ✔
- Проверка: варианты default/signed/ota=no/graph=no — задачи 8, 10; `template-check --quick` — задача 9. ✔

**Плейсхолдеры:** нет.

**Согласованность имён:** `FakeLink::{of, announcing}`, поля `next`/`fail_at`/`finish`/`finished`; `FakeFlash::{new, with_image}`, поля `memory`/`capacity`/`word`/`busy`/`prepared`/`writes`/`version`/`key`/`verify`/`verified`/`mark_updated`/`updated` — одинаковы в задачах 2–6. `Mode { check, apply }`, `cycle(link, flash, &mode) -> Result<(), S::Error>` — задачи 5–6. `Inputs { link, flash }` — задачи 5, 7, 8. `OtaLink`/`OtaFlash` — задачи 7, 8, 9. `VerifyError::{BadSignature, Flash}` — задачи 3, 5, 6.
