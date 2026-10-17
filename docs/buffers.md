# Буферы для периферии: zero-copy DMA-буфер

Некомпилируемый (`ignore`) пример — `cross`-workspace не собирается в
сыром шаблоне без подстановки `{{chip_feature}}` при генерации. Своего
модуля под буферы в шаблоне нет: статика заводится там, где собирается
драйвер, которому она нужна, — в `crates-cross/bsp/src/board.rs` или рядом
с ним.

## Рецепт: `bbqueue`

**Зависимости на `bbqueue` в шаблоне нет** (её удалили: никто не использовал) —
это рецепт «как подключить самому», а не описание того, что в проекте. Добавлять
крейт — только с разрешения владельца проекта (`docs/conventions.md`).

Grant/commit API: DMA пишет напрямую в backing storage, без промежуточного
копирования CPU. Не дублирует `embassy_sync::Pipe` (тот copy-based, для
обычного межзадачного байтового обмена без DMA). Статика создаётся только в
`cross`. Подключать с `default-features = false, features =
["critical-section"]` — default-фича `maitake-sync-0_3` (async-ожидание
грантов) не нужна для синхронного grant/commit ниже и лишь увеличивает размер
прошивки.

Пример под API 0.7. Блок `ignore`, компилятор его не трогает, поэтому
версию в нём никто не сверяет: подключаете `bbqueue` — перечитайте и это.

`Inline<N>` — буфер прямо в статике (без аллокатора), `AtomicCoord` —
курсоры на атомиках, `Polling` — без async-уведомлений, ровно под
синхронный цикл ниже.

```ignore
use bbqueue::{
    queue::BBQueue,
    traits::{coordination::cas::AtomicCoord, notifier::polling::Polling, storage::Inline},
};

static UART_TX: BBQueue<Inline<256>, AtomicCoord, Polling> = BBQueue::new();

let producer = UART_TX.stream_producer();
let consumer = UART_TX.stream_consumer();

let mut grant = producer.grant_exact(64).unwrap();
// DMA пишет прямо в grant — это `&mut [u8]`.
grant.commit(64);

let read = consumer.read().unwrap();
let sent = read.len();
read.release(sent);
```
