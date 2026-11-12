Внутренняя EEPROM чипа как носитель настроек: `ConfigFlash` над замком того же
контроллера флеша.

Когда настройки лежат в EEPROM, флеш под них не тратится вовсе. EEPROM
пишется через тот же контроллер, что и флеш, поэтому доступ к ней идёт через
общий `Lock` из `bsp::flash` — объект `Flash` на чипе один
(`flash::new`), и EEPROM получает ссылку на него, а не второй контроллер.

- `EepromFlash` — `Lock` как `Storage`/`ReadStorage`: операция под замком, байт
  за байтом, через `embassy_stm32::flash::Flash::eeprom_*`. Свой тип, а не
  `impl` на мьютексе: и трейт `Storage`, и `Mutex` чужие для `bsp`.
- `ConfigFlash` — `adapters_hw::eeprom::Eeprom<EepromFlash>`: инверсия байтов
  (EEPROM L0/L1 читается нулями, а `sequential-storage` ждёт `0xFF`) и страница
  стирания, на которую делится весь объём EEPROM.
- `config` собирает носитель для поля `config` платы (`Board::new`); на весь
  объём EEPROM над ним compose-site узла-потребителя строит
  `domain::settings::Settings`.

`NoopRawMutex` у `Lock` подразумевает один исполнитель — как и у остальных
носителей флеша; подробности — `docs/flash.md`.
