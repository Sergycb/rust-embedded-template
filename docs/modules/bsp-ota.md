Обвязка OTA этой платы: разделы из символов `memory.x`, версия образа и ключ.

Сам адаптер — `adapters::ota` (generic по `NorFlash`, тестируется на хосте),
логика приёма и применения — `domain::download` и `domain::update`. Здесь
остаётся ровно то, что привязано к чипу и сборке: `FirmwareUpdaterConfig::
from_linkerfile_blocking` (границы `DFU`/`BOOTLOADER_STATE` из линкерных
символов), размер `ACTIVE` как вместимость, версия из `build.rs` и открытый
ключ из `ota-public-key.bin`, — и псевдоним `Ota`, потому что задачи embassy
не могут быть generic.

Транспорт остаётся снаружи и сводится к порту `ports::ImageSource` под ваш
канал (USB CDC, UART-протокол, сеть, SD-карта) и двум вызовам:

```ignore
// Приём: сверка длины с вместимостью ДО стирания, `prepare` один раз,
// буферизация до слова флеша, контроль обещанной длины — всё внутри.
let len = domain::download::receive(&mut link, &mut board.ota, link.announced_len()).await?;
{%- if signed == "true" %}
// Подпись и длина приходят по тому же каналу, что и образ. Отказать вызов
// может по шести разным причинам, и различать их стоит: `UpdateError::
// Rollback` — прислали прошивку не новее текущей, `NoPublicKey` — ключ ещё
// не создан, `Flash(Error::Signature(_))` — подпись не сошлась.
domain::update::apply_signed(&mut board.ota, &signature, len)?;
{%- else %}
// Без подписи применять нечего — только попросить bootloader об обмене.
use ports::FirmwareUpdate;
board.ota.mark_updated()?;
{%- endif %}
cortex_m::peripheral::SCB::sys_reset();
```

Подтверждение образа (`mark_booted`) и то, почему без него обновление живёт
один запуск, описано в `adapters::ota`; вызов стоит в `main` и его стоит
перенести туда, где устройство доказало работоспособность.
