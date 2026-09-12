# Узел `OTA` в `domain` и правило «`lib.rs` только объявляет модули»

Дата: 2026-09-12

## Контекст

Задача от пользователя, дословно: «все lib.rs должны только объявлять модули
… в domain в каждом модуле делать задачу с supervisor_fragment. Тогда
пользователю проще будет работать, ему останется только сделать в bsp нужный
объект и отправить в app в задачу».

Что показала проверка посылок:

- `adapters/src/lib.rs` уже только объявляет модули (`ota`, `flash`,
  `settings`, `mem_flash`); единственный `lib.rs` с определениями —
  `ports/src/lib.rs` (286 строк: четыре трейта и два enum'а ошибок).
  `domain/src/lib.rs` держит inline-модуль `test_support` (15 строк,
  `cfg(test)`); `bsp/src/lib.rs` — модули и `pub use`.
- Буквальное «задача в каждом модуле `domain`» не имеет смысла: `firmware` —
  три `const fn`, `update::apply_signed` — одноразовый sync-вызов после
  приёма, `app::run` — уже задача. Осмысленное прочтение одно: **узел `OTA`**,
  который в цикле принимает образ (`download::receive`) и применяет его,
  а проект отдаёт ему объект канала.
- `supervisor` (rust-lib `647505b`) умеет **собственную `boot:`-привязку
  фрагмента**: фрагмент объявляет `boot: inputs: <Type>;`, compose-site
  кормит её в `fragments: [X = <expr>]`. Ровно то, что нужно, чтобы
  `domain` объявил узел с ресурсами, а `graph.rs` отдал ему поля `Board`.
  Корневой `Cargo.lock` шаблона пинит `supervisor` на 14 коммитов раньше;
  все 14 — эта фича, ничего ломающего.

Решения, принятые с пользователем в ходе брейнсторма (в порядке вопросов):

1. Узел `OTA` **сам владеет каналом** (а не получает команду от отдельной
   задачи-транспорта): `ImageSource` расширяется методом `begin()`.
2. Signed/unsigned выбирается **при генерации** существующим плейсхолдером
   `signed`: в `domain` две точки входа (`run`/`run_signed`) и два фрагмента,
   `graph.rs` подбирает нужный Liquid'ом. Обе функции остаются в исходнике
   `domain` сгенерированного проекта — Liquid в `domain` запрещён
   (`docs/architecture.md`), а неиспользуемая generic-функция во flash не
   попадает (прецедент: `firmware`/`update` в проекте без подписи).
3. Что делать с исходом — ack хосту, сброс МК, и то и другое — **решает
   реализация порта**: `ImageSource::finish(outcome)`, узел только зовёт.
4. Правило для `lib.rs`: атрибуты крейта, `//!`, `mod`, `pub use` —
   реэкспорты в корне остаются, пути `ports::X` не меняются.

## Часть A — `lib.rs` и разбиение `ports`

### Правило (в `docs/conventions.md`)

`lib.rs` содержит только: атрибуты крейта (`#![no_std]`, …),
`//!`-документацию крейта, объявления `mod` с doc-комментариями и
`pub use`-реэкспорты. Определений — типов, трейтов, функций, `impl` — в нём
нет. Реэкспорт в корне — норма экосистемы (`embedded-hal`, `embassy-*`):
короткий путь `ports::FirmwareUpdate` остаётся, внутреннее деление на модули
пользователя не касается.

### `ports` — три модуля, зеркально `domain`, ошибка рядом со своим трейтом

| Файл | Содержимое |
|---|---|
| `ports/src/update.rs` | `FirmwareUpdate`, `SignedFirmwareUpdate`, `UpdateError`, новый `VerifyError` |
| `ports/src/download.rs` | `ImageSource`, новые `Announce`, `Rejection`, `DownloadError` |
| `ports/src/settings.rs` | `SettingsStorage` |
| `ports/src/lib.rs` | прежний `//!`-заголовок, три `pub mod` с doc-комментариями, `pub use` каждого публичного типа в корень |

Модули названы по операции домена, которая ими пользуется (`download`,
`update`, `settings`), а не по трейту: `DownloadError<S, F>` — ошибка
операции `domain::download::receive`, а не одного порта, и класть её
«к трейту» было бы не к чему.

### `domain/src/lib.rs`

`test_support` уезжает в `domain/src/test_support.rs`
(`#[cfg(test)] pub(crate) mod test_support;`). Туда же переезжают и
объединяются фейки портов из тестов `download.rs` и `update.rs`: один
`FakeFlash`, реализующий `FirmwareUpdate` **и** `SignedFirmwareUpdate`
(настраиваемые версия, ключ и исход проверки подписи; записывает байты и
помнит, звались ли `prepare`/`mark_updated`/`verify_and_mark_updated`), и один
`FakeLink` — сценарий из заголовков и кусков, запись всех `finish`. Новому
модулю `ota` нужны оба, и три копии фейков — ровно то дублирование, от которого
уходит правило одного фейка на порт.

`bsp/src/lib.rs` правилу уже соответствует и не меняется.

## Часть B — порт канала, порт подписи, узел

### `ports::download` — `ImageSource` становится полным протоколом

```rust
/// Заголовок образа, который канал получил от отправителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Announce {
    pub len: u32,
    /// `None` — канал подписи не передал. Проект с подписью откажет до
    /// стирания раздела; проект без подписи поле не читает.
    pub signature: Option<[u8; 64]>,
}

/// Код отказа для отправителя. Плоский и `Copy` намеренно: `ImageSource` не
/// знает тип ошибки флеша, а отправителю нужен код, на который он может
/// отреагировать, а не payload. Полная ошибка уходит в лог узла.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Rejection {
    /// Заявленная длина негодная: ноль, больше раздела, короче хвоста с версией.
    Length,
    /// Передача не сошлась с заголовком: байт больше или меньше обещанного.
    Transfer,
    /// В разделе образ, который терять нельзя (обмен ждёт сброса или ещё не
    /// подтверждён) — сначала перезагрузка.
    Busy,
    /// Версия не новее текущей.
    Rollback,
    /// Подписи нет, ключ не подставлен или подпись не сошлась.
    Signature,
    /// Отказ флеша или адаптера на стороне устройства.
    Device,
}

pub trait ImageSource {
    type Error;
    /// Ждёт заголовок следующего образа — сколько угодно долго.
    async fn begin(&mut self) -> Result<Announce, Self::Error>;
    /// Следующий кусок образа; `None` — передача окончена. (Как сейчас.)
    async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error>;
    /// Исход цикла. Что с ним делать — ответить хосту, сбросить МК, и то и
    /// другое — решает реализация; узел после `Ok(())` ждёт следующий `begin`.
    async fn finish(&mut self, outcome: Result<(), Rejection>) -> Result<(), Self::Error>;
}
```

### `ports::update` — `VerifyError`

Сейчас `verify_and_mark_updated` возвращает `Self::Error`, и «подпись не
сошлась» неотличима от «флеш умер»: узел был бы вынужден слать `Device`
вместо `Signature`. Поэтому:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum VerifyError<E> {
    #[error("подпись не сошлась")]
    BadSignature,
    #[error("отказ адаптера обновления: {0:?}")]
    Flash(E),
}

// SignedFirmwareUpdate:
fn verify_and_mark_updated(&mut self, signature: &[u8; 64], len: u32)
    -> Result<(), VerifyError<Self::Error>>;
```

Без `#[non_exhaustive]` (в отличие от `Rejection`) и с `Clone, Copy`: `domain` —
другой крейт, а не `ports`, и обязан отобразить оба варианта exhaustive-`match`'ем;
`Copy` — чтобы фейк в тестах мог вернуть значение из `&mut self`.

`adapters::ota::Signed` маппит `FirmwareUpdaterError::Signature(_)` →
`BadSignature`, остальное → `Flash`. `UpdateError` получает вариант
`BadSignature`; `apply_signed` перекладывает `VerifyError` в него. Существующий
тест `passes_a_signature_failure_through` меняет ожидание на
`UpdateError::BadSignature`.

### `domain::ota` — узел

```rust
/// Что узлу нужно на входе; строит compose-site из полей `Board`.
pub struct Inputs<S, F> { pub link: S, pub flash: F }

pub async fn run<S: ImageSource, F: FirmwareUpdate>(link: &mut S, flash: &mut F) -> TaskExit;
pub async fn run_signed<S: ImageSource, F: SignedFirmwareUpdate>(link: &mut S, flash: &mut F) -> TaskExit;
```

Обе — тонкий `loop` над приватным **одним циклом** (единица, которую
проверяют тесты): `begin` → проверки до стирания → `receive` → применение →
`finish`. Режимы различаются двумя шагами и оформляются как пара небольших
функций (`check`/`apply`), а не публичной абстракцией.

Один цикл:

1. `link.begin()` → `Announce`.
2. **До стирания раздела**, в этом порядке:
   `flash.is_busy()?` → `Rejection::Busy` (адаптер и так откажет в `prepare`,
   но тогда отправитель получил бы `Device`); в `run_signed`:
   `announce.signature.is_none()` → `Rejection::Signature`.
3. `download::receive(link, flash, announce.len)`.
4. Применение: `run` — `flash.mark_updated()`; `run_signed` —
   `update::apply_signed(flash, &signature, len)`.
5. `link.finish(outcome)`.

Отображение ошибок в код отказа:

| Откуда | Вариант | `Rejection` |
|---|---|---|
| `DownloadError` | `Empty`, `TooLong` | `Length` |
| | `TooMuchData`, `Incomplete` | `Transfer` |
| | `UnusableGranularity`, `Flash(_)` | `Device` |
| | `Source(_)` | — канал сломан, см. ниже |
| `UpdateError` | `TooLong`, `Truncated` | `Length` |
| | `Busy` | `Busy` |
| | `Rollback` | `Rollback` |
| | `NoPublicKey`, `BadSignature` | `Signature` |
| | `Flash(_)` | `Device` |
| `mark_updated()` / `is_busy()` | `Err(_)` | `Device` |

Любой отказ из таблицы: полная ошибка в лог (`warn!`, через `Debug` —
отсюда bounds `S::Error: Debug`, `F::Error: Debug` на `run*`), затем
`finish(Err(код))`, и цикл продолжается. Ошибка **самого канала** — из
`begin`, `next` (`DownloadError::Source`) или `finish` — это `TaskExit::Failed`:
граф перезапустит узел с backoff'ом, объект канала вернётся в слот и уедет в
новый прогон. `Completed` не возвращается никогда: после удачного обновления
узел ждёт следующий образ (если реализация `finish` не сбросила МК).

**Без `watchdog:`.** Узел законно висит в `begin()` сколько угодно, а
опрашивать его с таймаутом нельзя: отмена future уронила бы полупрочитанный
заголовок (cancel-safety, `docs/modules/app-graph.md`). Железо кормит узел
`APP`.

Фрагменты — два, потому что Liquid в `domain` запрещён:

```rust
supervisor::supervisor_fragment! {
    name: OTA_FRAG;
    boot: inputs: $crate::ota::Inputs<OtaLink, OtaFlash>;
    node OTA, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
        resources: [LINK: local OtaLink = inputs.link, FLASH: local OtaFlash = inputs.flash],
        task: $crate::ota::run(ctx.link, ctx.flash);
}

supervisor::supervisor_fragment! {
    name: OTA_SIGNED_FRAG;
    boot: inputs: $crate::ota::Inputs<OtaLink, OtaFlash>;
    node OTA, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
        resources: [LINK: local OtaLink = inputs.link, FLASH: local OtaFlash = inputs.flash],
        task: $crate::ota::run_signed(ctx.link, ctx.flash);
}
```

Оба слота — `local`: объекты платы `!Send` (`bsp::ota::Ota` держит
`&'static Mutex<NoopRawMutex, …>`), а обычный слот графа — `static`, которому
нужен `Send`.

`OtaLink`, `OtaFlash`, `RestartPolicy`, `backoff()` резолвятся на
compose-site. Для констант политики это уже прецедент (`APP_WATCHDOG`); для
типов — его расширение, и оно записывается в `docs/architecture.md`: тип
ресурсного слота — `static`, назвать его фрагмент обязан, а конкретный тип
(`bsp::ota::Ota`, транспорт проекта) `domain` не видит и видеть не должен.

### `crates-cross` и генерация

- Корневой `Cargo.lock`: `cargo update -p supervisor`.
- **`bsp/src/ota.rs`: заглушка `pub struct Link`**, реализующая
  `ImageSource` с `type Error = Infallible`: `begin` — один `defmt::warn!`
  «транспорт OTA не реализован» и `core::future::pending().await`; `next` —
  `Ok(None)`; `finish` — `Ok(())`. `Board` получает поле `ota_link: Link`.
  Это каркас, как `app::run`: проект переписывает тело под свой USB/UART.
  Заглушка, а не «добавьте сами», ради проверяемости: `template-check`
  компилирует узел `OTA` в обоих вариантах (signed/unsigned) — иначе фрагменты
  в `domain` никем не раскрываются и сгниют незаметно.
- `app/src/graph.rs`, под `{% if ota == "true" %}`:
  `type OtaLink = bsp::ota::Link; type OtaFlash = bsp::ota::Ota;` с
  комментарием «эти два имени называет фрагмент `domain::OTA*_FRAG`», и
  `fragments: [::domain::APP_FRAG, ::domain::OTA{% if signed == "true" %}_SIGNED{% endif %}_FRAG = ::domain::ota::Inputs { link: board.ota_link, flash: board.ota }];`
  Привязка фрагмента эмитится первой строкой пролога `spawn_all`
  (частичный move `board.ota_link`, `board.ota`), затем
  `watchdog: = board.watchdog.arm()` — другое поле, `Board` без `Drop`,
  законно.
- `app/src/main.rs`: `board.ota.mark_booted()` остаётся как есть
  (заимствование до move `board` в `spawn_all`).
- Вариант `graph=no, ota=yes`: узел не собирается, заглушка `Link` в `Board`
  есть; документация говорит «зовите `domain::ota::run(&mut board.ota_link,
  &mut board.ota)` сами».

## Тесты

Host, `domain/src/ota.rs`, на `FakeLink`/`FakeFlash` из `test_support`,
через `block_on` (фейки не ждут). Единица — один цикл; `run*` проверяется
одним сценарием на выход `Failed`.

- unsigned, happy path: `prepare(len)` один раз, байты на месте,
  `mark_updated` позван, `finish(Ok)`.
- signed, happy path: `verify_and_mark_updated` получил подпись из заголовка
  и `len`, `finish(Ok)`.
- `is_busy` → `finish(Err(Busy))`, `prepare` не звался.
- signed без подписи → `finish(Err(Signature))`, `prepare` не звался.
- `len` больше раздела → `Length`, `prepare` не звался.
- кусков больше заявленного → `Transfer` (после стирания — так и должно).
- откат версии → `Rollback`; `BadSignature` → `Signature`; ошибка флеша при
  `mark_updated` → `Device`.
- ошибка канала в `begin` / в `next` / в `finish` → `TaskExit::Failed`,
  `finish` при обрыве в `next` не звался.

`adapters`: тест на маппинг `FirmwareUpdaterError::Signature` → `BadSignature`
(под `signed`). `update.rs`: `passes_a_signature_failure_through` меняет
ожидание. `download.rs`: тесты переезжают на общие фейки без изменения
сценариев.

## Документация

- `docs/conventions.md` — правило `lib.rs`.
- `docs/architecture.md` — узел `OTA` как второй прецедент «узлы — в
  `domain`»; имена типов, резолвящиеся на compose-site; `Board.ota_link`.
- `docs/modules/app-graph.md` — раздел «OTA: что делает шаблон и что остаётся
  вам» переписывается: остаётся реализация `ImageSource` в `bsp`.
- `docs/ota.md` — поток через узел вместо ручного `receive`; `VerifyError`.
- `README.md` — раздел OTA: пример ручного `receive`/`apply_signed` заменяется
  на «реализуйте три метода `ImageSource` в `bsp::ota::Link`».
- `docs/README.md` — при необходимости строка про узел.
- `cargo-generate.toml` — `docs/superpowers` в `ignore` (спеки мейнтейнера с
  кусками Liquid, как `.superpowers`).

## Проверка

1. `cargo xtask lint`, `cargo xtask test host` — в репозитории шаблона.
2. Генерация и `cargo xtask lint cross` + `cargo xtask build` в вариантах
   `default` (ota, unsigned), `signed=yes`, `ota=no`, `graph=no` — каждый со
   своим `CARGO_TARGET_DIR`; полный `template-check` — по решению
   пользователя (≈30–40 мин).
3. Плата не нужна.

## За рамками

- `domain::app::run` по-прежнему без host-теста.
- `bsp::config`/`settings` узла не получают: у них нет цикла, это объект для
  чтения/записи из задач проекта.
- Раздельный код отказа для «нет подписи» vs «подпись не сошлась» — оба
  `Signature`; при необходимости расширяется (`#[non_exhaustive]`).
