Паникёр и транспорт лога бинарника: в dev — `panic-probe`, в release — дамп в
переживающую сброс память и сброс МК; лог — `defmt-rtt`. У обоих ровно по
одному на бинарник, и оба приходят из одного места — крейта
`crates-cross/panic-handler`.

Здесь, а не в `bsp`: что делать с паникой — решение прошивки, а не плата; и не
в библиотеке `app`: её линкует `target-tests`, где паникёр даёт
`embedded-test`. Бинарники подключают крейт (`use panic_handler as _`),
`target-tests` — нет. Состояние паники (что читают сведения об устройстве) —
`app::panic`.
{% if ota == "true" %}У bootloader'а паникёр свой, без сообщений (`crates-cross/boot`), у
`target-tests` — свой от `embedded-test`.{% else %}У `target-tests` паникёр свой — от `embedded-test`.{% endif %}
