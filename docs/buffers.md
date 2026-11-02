# Буферы для периферии: zero-copy DMA-буфер

Некомпилируемый (`ignore`) пример: крейта `bbqueue` в шаблоне нет, а DMA-канал
под него выбирает плата. Своего модуля под буферы в шаблоне нет: очередь под
DMA, без которой драйвер платы не собрать, — статика `bsp` рядом с драйвером
(`crates-cross/bsp/src/board.rs` или рядом с ним); буфер сообщений протокола
над рычагом — в `app` (docs/architecture.md, «`Board` отдаёт рычаги платы, а не
периферию и не протоколы»).

## Рецепт: `bbqueue`

**Зависимости на `bbqueue` в шаблоне нет** (её удалили: никто не использовал) —
это рецепт «как подключить самому», а не описание того, что в проекте. Добавлять
крейт — только с разрешения владельца проекта (`docs/conventions.md`).

Grant/commit API: DMA пишет напрямую в backing storage, без промежуточного
копирования CPU. Не дублирует `embassy_sync::Pipe` (тот copy-based, для
обычного межзадачного байтового обмена без DMA). Статика создаётся только в
`cross`. Подключать с `default-features = false` — default-фича
`maitake-sync-0_3` (async-ожидание грантов) не нужна для синхронного
grant/commit ниже и лишь увеличивает размер прошивки. Координация — по ядру:
`coordination::cas::AtomicCoord` (CAS-атомики, фичи не нужны) или, на
Cortex-M0/M0+ без CAS, `coordination::cs::CsCoord` — ему нужна фича
`critical-section`.

Пример под API 0.7. Блок `ignore`, компилятор его не трогает, поэтому
версию в нём никто не сверяет: подключаете `bbqueue` — перечитайте и это.

`Inline<N>` — буфер прямо в статике (без аллокатора), {% if core_baseline == "true" %}`CsCoord` —
курсоры под критической секцией (CAS у этого ядра нет){% else %}`AtomicCoord` —
курсоры на атомиках{% endif %}, `Polling` — без async-уведомлений, ровно под
синхронный цикл ниже.

```ignore
use bbqueue::{
    queue::BBQueue,
    traits::{coordination::{% if core_baseline == "true" %}cs::CsCoord{% else %}cas::AtomicCoord{% endif %}, notifier::polling::Polling, storage::Inline},
};

static DMA_TX: BBQueue<Inline<256>, {% if core_baseline == "true" %}CsCoord{% else %}AtomicCoord{% endif %}, Polling> = BBQueue::new();

let producer = DMA_TX.stream_producer();
let consumer = DMA_TX.stream_consumer();

let mut grant = producer.grant_exact(64).unwrap();
// DMA пишет прямо в grant — это `&mut [u8]`.
grant.commit(64);

let read = consumer.read().unwrap();
let sent = read.len();
read.release(sent);
```
{%- if cache contains "scb_d" %}

**На этом чипе кэш данных ядра стоит перед RAM, а DMA его не видит**
(`docs/clocks.md`, «Кэши»): очередь под DMA из примера выше так не годится.
Хранилище кладите в регион `.dma_nc`{% if bdma_nc == "true" %} (для BDMA — `.bdma_nc`){% endif %}, а саму очередь — над
ссылкой на него. Целиком `BBQueue` туда не положить: секция `NOLOAD`, и курсоры
остались бы неинициализированными. `new_with_storage` не `const`, поэтому
очередь — в `StaticCell` вне региона:

```ignore
#[unsafe(link_section = ".dma_nc")]
static STORAGE: Inline<256> = Inline::new();
static DMA_TX: StaticCell<BBQueue<&'static Inline<256>, AtomicCoord, Polling>> = StaticCell::new();

let queue = DMA_TX.init(BBQueue::new_with_storage(&STORAGE));
let producer = queue.stream_producer();
let consumer = queue.stream_consumer();
```
{%- endif %}
