Обвязка OTA этой платы: адаптер разделов из символов `memory.x` и канал
доставки — вместе полем `Board::ota` ([`Ota`], вход узла `domain::ota`).

Сам адаптер — `adapters::ota` (generic по `NorFlash`, тестируется на хосте),
логика приёма и применения — `domain::ota::download` и `domain::ota::update`, а зовёт
их узел `domain::ota`. Здесь остаётся ровно то, что привязано к чипу:
`FirmwareUpdaterConfig::from_linkerfile` (границы `DFU`/`BOOTLOADER_STATE`
из линкерных символов), размер `ACTIVE` — его порт отдаёт методом
`FirmwareUpdate::capacity`, — и псевдонимы типов, потому что задачи embassy
не могут быть generic.{% if ota_store == "external" %} Здесь `DFU` и `BOOTLOADER_STATE` — на внешней флеши:
оба раздела строятся над одним драйвером из `crate::external`, символы в
`memory.x` — смещения от её начала (docs/flash.md).{% else %} `BOOTLOADER_STATE` строится не над цельным `Flash`, как
`DFU`, а над `adapters::flash::Paged` со страницей своего сектора
(`STATE_ERASE_SIZE`) — почему, см. docs/flash.md. Оба раздела асинхронные:
общий блокирующий `Flash` виден им через мост `adapters::flash::Shared`, а
порт `FirmwareUpdate` — асинхронный, чтобы за ним могла стоять и внешняя
флеш.{% endif %}{% if signed == "true" %} Адаптер собран `Updater::signed` — его
`prepare` стирает и запасную страницу `DFU` под запись о подписи; версию
образа задаёт приложение (`crates-cross/app/src/image.rs`), ключ —
bootloader.{% endif %}

Транспорт — поле `link` в `Board::ota`: в шаблоне это заглушка [`Link`], которая
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
`domain::ota`{% if graph == "true" %}, который граф спавнит из `fragments:`
(`crates-cross/app/src/graph.rs`){% else %}. Без графа его спавнит `main`
(`ota_task` над `domain::ota::task_standalone`), отдав задаче `link` и `flash`
владением{% endif %}. Во фрагменте
`domain::ota` слоты канала и адаптера помечены `local`: [`Updater`] — `!Send` (внутри ссылка на {% if ota_store == "external" %}`crate::external::ExternalMutex`{% else %}`FlashMutex`{% endif %} с
`NoopRawMutex`), и граф держит его на своём исполнителе, не требуя `Send`, —
то же допущение «один исполнитель», что у самого `NoopRawMutex`; захотите
вынести OTA на другой исполнитель — меняйте `local` во фрагменте
`domain::ota` и тип мьютекса вместе.

Подтверждение образа (`mark_booted`) и то, почему без него обновление живёт
один запуск, описано в `adapters::ota`; зовёт его сам узел `domain::ota` при
старте (действие `begin`), и его стоит перенести туда, где устройство доказало
работоспособность. Не в `main` до исполнителя — порт асинхронный, и драйвер
внешней флеши может ждать таймер или прерывание.
