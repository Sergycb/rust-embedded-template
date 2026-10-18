Флеш платы: общий замок над контроллером{% if ota == "true" %} и разделы обновления прошивки (`Board::partitions`){% endif %}.

Контроллер флеша на чипе один, а работать с ним нужно всем сразу{% if ota == "true" and ota_store != "external" %}: разделам
`DFU` и `BOOTLOADER_STATE` обновления{% endif %}{% if config == "true" and config_store == "flash" %}, разделу `CONFIG` настроек{% elsif config == "true" and config_store == "eeprom" %}, EEPROM настроек (она пишется через тот же контроллер){% endif %}. Поэтому объект
создаётся ровно один раз (`flash::chip`), лежит в `static` под блокирующим
замком (`FlashMutex`), а потребители получают на него ссылки через
`Whole` (`adapters::flash::whole` — раздел `embassy-embedded-hal` на весь флеш); асинхронный вид для `embassy-boot` и
`sequential-storage` даёт `embassy_embedded_hal::adapter::BlockingAsync`.
`NoopRawMutex` подразумевает один исполнитель — подробности в `docs/flash.md`.
{%- if ota == "true" %}

## Разделы обновления

`Board::partitions` ([`Partitions`]) — то, что плата знает про раскладку флеша,
и ничего сверх этого: раздел `DFU`, раздел `BOOTLOADER_STATE` (оба найдены по
линкерным символам `memory.x`, `FirmwareUpdaterConfig::from_linkerfile`) и
размер `ACTIVE`. Алгоритм обновления над ними — адаптер
`adapters::ota::Updater` (generic по `NorFlash`, тестируется на хосте),
приём и применение образа — `domain::ota` — железа не касается, поэтому
собирает его `app`: `updater()` в `crates-cross/app/src/main.rs`.
Псевдонимы `Dfu` и `State` нужны, чтобы `app` мог назвать тип адаптера
(задачи embassy не могут быть generic).
{%- if ota_store == "external" %}

Здесь `DFU` и `BOOTLOADER_STATE` — на внешней флеши: оба раздела строятся над
одним драйвером из `crate::external`, символы в `memory.x` — смещения от её
начала (docs/flash.md).
{%- else %}

`BOOTLOADER_STATE` строится не над цельным `Flash`, как `DFU`, а над
`adapters::flash::ErasePage` со страницей своего сектора (`STATE_ERASE_SIZE`) —
почему, см. docs/flash.md. Число приезжает переменной среды `STATE_ERASE_SIZE`
из `[env]` `crates-cross/.cargo/config.toml` — тем же значением её читает
bootloader (он от `bsp` не зависит), и разделам в приложении и в нём расходиться
нельзя. Нет переменной — страница цельного `Flash`.
{%- if swap_paged == "true" %} Раздел `DFU` строится тоже не над цельным `Flash`, а над
`ErasePage` со страницей обмена `SWAP_PAGE_SIZE` (переменная среды, та же, что у
bootloader'а) — она крупнее физической страницы флеша этого чипа: так журнал
`BOOTLOADER_STATE` (запись на страницу `ACTIVE`) короче, и коду bootloader'а
остаётся почти весь его блок. `ACTIVE` с той же страницей собирает bootloader.
Кратность странице стирания флеша проверяется при компиляции.
{%- endif %}
{%- endif %}
{%- if signed == "true" %}

Адаптер собирается как `Updater::signed` — его `prepare` стирает и запасную
страницу `DFU` под запись о подписи; версию образа задаёт приложение
(`crates-cross/app/src/image.rs`), ключ — bootloader.
{%- endif %}

`active_len` — это размер `ACTIVE`, а не `DFU`: образ принимается в `DFU`, но
обмен копирует ровно `ACTIVE`, и образ длиннее него приехал бы обрезанным
(подробнее — у поля).

## Свой транспорт

Канал доставки образа — не железо платы, а слот приложения: `Link` и `link()`
в `crates-cross/app/src/main.rs`. В шаблоне там заглушка
`adapters::ota::Unconnected`, которая ждёт заголовок вечно. Заменить её —
значит реализовать три метода `ports::ImageSource` на объекте, собранном из
вашей периферии (`app` зависит от `ports` не по умолчанию: добавьте
`ports = { workspace = true }` в `crates-cross/app/Cargo.toml`), и поменять
псевдоним `Link` и `link()`:

```ignore
impl ImageSource for MyLink {
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

Транспорт собирается из периферии платы, а периферия не выходит из `bsp`
наружу: если ему нужен UART или USB, который уже отдаёт `Board`, возьмите
объект оттуда, а свой блок положите в группу `resources.rs`
(docs/resources.md).

Всё остальное — сверка длины до стирания, `prepare` один раз, буферизация до
слова флеша, порядок проверок подписи, коды отказа — уже в узле
`domain::ota`{% if graph == "true" %}, который граф спавнит из `fragments:`
(`crates-cross/app/src/graph.rs`){% else %}. Без графа его спавнит `main`
(`ota_task` над `domain::ota::task_standalone`), отдав задаче `link` и `flash`
владением{% endif %}. Во фрагменте
`domain::ota` слоты канала и адаптера помечены `local`: `Updater` — `!Send` (внутри ссылка на {% if ota_store == "external" %}`crate::external::ExternalMutex`{% else %}`FlashMutex`{% endif %} с
`NoopRawMutex`), и граф держит его на своём исполнителе, не требуя `Send`, —
то же допущение «один исполнитель», что у самого `NoopRawMutex`; захотите
вынести OTA на другой исполнитель — меняйте `local` во фрагменте
`domain::ota` и тип мьютекса вместе.

Подтверждение образа (`mark_booted`) и то, почему без него обновление живёт
один запуск, описано в `adapters::ota`; зовёт его сам узел `domain::ota` при
старте (действие `begin`), и его стоит перенести туда, где устройство доказало
работоспособность. Не в `main` до исполнителя — порт асинхронный, и драйвер
внешней флеши может ждать таймер или прерывание.
{%- endif %}
