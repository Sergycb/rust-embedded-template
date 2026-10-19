Обновление прошивки платы: адаптер разделов и канал доставки образа —
одним полем `Board::ota` ([`Ota`]): канал и адаптер обновления в одном объекте
(`adapters::ota::Ota`, порт `OtaDevice`).

Адаптер обновления ([`Updater`]) — `adapters::ota::Updater` `adapters::ota::Updater` (generic по
`NorFlash`, тестируется на хосте), реализация порта `FirmwareUpdate` поверх
разделов `DFU` и `BOOTLOADER_STATE`. Разделы `bsp` находит по линкерным
символам `memory.x` (`FirmwareUpdaterConfig::from_linkerfile`) — это и есть то,
что привязано к чипу. Логика приёма и применения — `domain::ota::download` и
`domain::ota::update` — работает только с портами, её зовёт узел
`domain::ota`. Псевдонимы типов нужны, потому что задачи embassy не могут быть
generic.
{%- if ota_store == "external" %} Здесь `DFU` и `BOOTLOADER_STATE` — на внешней флеши:
оба раздела строятся над одним драйвером из `crate::external`, символы в
`memory.x` — смещения от её начала (docs/flash.md).{% else %}{% if state_paged == "true" %} `BOOTLOADER_STATE` строится не над цельным `Flash`, как
`DFU`, а над `adapters::flash::ErasePage` со страницей своего сектора
(`STATE_ERASE_SIZE`) — почему, см. docs/flash.md.{% else %} Сектор `BOOTLOADER_STATE` равен странице
цельного `Flash`, поэтому раздел строится прямо над ним, как `DFU` (равенство
проверяет `const`-assert).{% endif %} Число приезжает переменной
среды `STATE_ERASE_SIZE` из `[env]` `crates-cross/.cargo/config.toml` — тем же
значением её берёт и bootloader (`bsp::ota::STATE_ERASE_SIZE`), и разделам в
приложении и в нём расходиться негде.{% if swap_paged == "true" %} Раздел `DFU` строится тоже не над цельным `Flash`, а над
`ErasePage` со страницей обмена `SWAP_PAGE_SIZE` (переменная среды, та же, что у
bootloader'а) — она крупнее физической страницы флеша этого чипа: так журнал
`BOOTLOADER_STATE` (запись на страницу `ACTIVE`) короче, и коду bootloader'а
остаётся почти весь его блок. `ACTIVE` с той же страницей собирает bootloader.
Кратность странице стирания флеша проверяется при компиляции.{% endif %}{% endif %}{% if signed == "true" %} Адаптер собран с `SIGNED = true` — его
`prepare` стирает и запасную страницу `DFU` под запись о подписи; версию
образа задаёт приложение (`crates-cross/app/src/image.rs`), ключ —
bootloader.{% endif %}

Размер `ACTIVE` адаптер получает числом — переменной среды `ACTIVE_LEN` из
`[env]`: тем же расчётом генерация пишет `ACTIVE` в `memory.x`. Это `ACTIVE`, а
не `DFU`: образ принимается в `DFU`, но обмен копирует ровно `ACTIVE`, и образ
длиннее него приехал бы обрезанным.

## Свой транспорт

Канал доставки образа — первая половина `Board::ota`, тип [`Link`]. В шаблоне там заглушка
`adapters::ota::Unconnected`, которая ждёт заголовок вечно. Заменить её —
значит реализовать три метода `ports::ImageSource` на объекте, собранном из
вашей периферии (`bsp` зависит от `ports` не по умолчанию: добавьте
`ports = { workspace = true }` в `crates-cross/bsp/Cargo.toml`), и поменять
псевдоним `Link` и функцию `link()`:

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

Транспорт собирается из периферии платы: возьмите блок в группу
`resources.rs` (docs/resources.md) и соберите объект в `link()`.

Всё остальное — сверка длины до стирания, `prepare` один раз, буферизация до
слова флеша, порядок проверок подписи, коды отказа — уже в узле
`domain::ota`{% if graph == "true" %}, который граф спавнит из `fragments:`
(`crates-cross/app/src/graph.rs`){% else %}. Без графа его спавнит `main`
(`ota::task` над `domain::ota::task` в `main.rs`), отдав задаче `ota`
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
