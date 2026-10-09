Обвязка OTA этой платы: адаптер разделов из символов `memory.x` и канал
доставки — вместе полем `Board::ota` ([`Ota`], вход узла `domain::ota`).

Сам адаптер — `adapters::ota` (generic по `NorFlash`, тестируется на хосте),
логика приёма и применения — `domain::download` и `domain::update`, а зовёт
их узел `domain::ota`. Здесь остаётся ровно то, что привязано к чипу:
`FirmwareUpdaterConfig::from_linkerfile_blocking` (границы `DFU`/`BOOTLOADER_STATE`
из линкерных символов), размер `ACTIVE` — его порт отдаёт методом
`FirmwareUpdate::capacity`, — и псевдонимы типов, потому что задачи embassy
не могут быть generic.{% if signed == "true" %} Версию образа и открытый ключ задаёт не
плата, а приложение: `crates-cross/app/src/image.rs` оборачивает адаптер в
`adapters::ota::Signed`.{% endif %}

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
(`crates-cross/app/src/graph.rs`){% else %}. Без графа зовите узел сами, собрав
его контекст руками:
`domain::ota::run(domain::ota::OtaCtx { link: &mut board.ota.link, flash: &mut {% if signed == "true" %}flash{% else %}board.ota.flash{% endif %}, mode: &mut domain::ota::Mode::{% if signed == "true" %}signed{% else %}plain{% endif %}(), index: 0 }).await`{% endif %}. Во фрагменте
`domain::ota` слоты канала и адаптера помечены `local`: [`Updater`] — `!Send` (внутри ссылка на `FlashMutex` с
`NoopRawMutex`), и граф держит его на своём исполнителе, не требуя `Send`, —
то же допущение «один исполнитель», что у самого `NoopRawMutex`; захотите
вынести OTA на другой исполнитель — меняйте `local` во фрагменте
`domain::ota` и тип мьютекса вместе.

Подтверждение образа (`mark_booted`) и то, почему без него обновление живёт
один запуск, описано в `adapters::ota`; вызов стоит в `main` и его стоит
перенести туда, где устройство доказало работоспособность.
