Обвязка OTA этой платы: разделы из символов `memory.x`{% if signed == "true" %}, версия образа и ключ{% endif %}.

Сам адаптер — `adapters::ota` (generic по `NorFlash`, тестируется на хосте),
логика приёма и применения — `domain::download` и `domain::update`. Здесь
остаётся ровно то, что привязано к чипу и сборке:
`FirmwareUpdaterConfig::from_linkerfile_blocking` (границы `DFU`/`BOOTLOADER_STATE`
из линкерных символов), размер `ACTIVE` как вместимость{% if signed == "true" %},
версия из `build.rs` и открытый ключ из `ota-public-key.bin`{% endif %}, — и
псевдоним `Ota`, потому что задачи embassy не могут быть generic.

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
`domain::ota`{% if graph == "true" %}, который граф спавнит из `fragments:`
(`crates-cross/app/src/graph.rs`){% else %}. Без графа зовите узел сами:
`domain::ota::run{% if signed == "true" %}_signed{% endif %}(&mut board.ota_link, &mut board.ota).await`{% endif %}. Во фрагментах
`domain::ota` слоты узла помечены `local`: `Ota` — `!Send` (внутри ссылка на `FlashMutex` с
`NoopRawMutex`), и граф держит его на своём исполнителе, не требуя `Send`, —
то же допущение «один исполнитель», что у самого `NoopRawMutex`; захотите
вынести OTA на другой исполнитель — меняйте `local` во фрагменте
`domain::ota` и тип мьютекса вместе.

Подтверждение образа (`mark_booted`) и то, почему без него обновление живёт
один запуск, описано в `adapters::ota`; вызов стоит в `main` и его стоит
перенести туда, где устройство доказало работоспособность.
