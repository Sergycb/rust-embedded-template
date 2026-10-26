//! Числа раскладки флеша, которые нужны кускам флеша платы (`bsp::flash`{% if external_flash == "true" %},
//! `bsp::external`{% endif %}), — одно место разбора на все.
//!
//! Приезжают переменными среды из `[env]` `crates-cross/.cargo/config.toml`:
//! тем же расчётом генерация пишет `memory.x`, и читать их из линкерных символов
//! (`unsafe extern`) незачем.{% if ota == "true" %} Исключение — отпечаток раскладки
//! ([`fingerprint`]): он сверяет с образом именно то, с чем слинкована прошивка,
//! и правка `memory.x` руками мимо `[env]` должна в нём отразиться.{% endif %}
{%- if ota == "true" %}

/// Размер `ACTIVE` в байтах: самый длинный образ, который доедет до
/// устройства. Это `ACTIVE`, а НЕ `DFU`, хотя принимается образ в `DFU`.
/// Раздел обновления по построению больше активного на одну-две страницы (этого
/// требует `assert_partitions` в `embassy-boot`: обмену нужна запасная
/// страница), а обмен копирует ровно `ACTIVE`. Образ размером между ними прошёл
/// бы и проверку длины, и подпись, и пометку к обмену — а в `ACTIVE` приехал бы
/// обрезанным: устройство отработало бы полный цикл обновления с перезагрузкой и
/// молча откатилось.
pub(crate) const ACTIVE_LEN: u32 = match u32::from_str_radix(env!("ACTIVE_LEN"), 10) {
    Ok(len) => len,
    Err(_) => panic!("ACTIVE_LEN в [env] crates-cross/.cargo/config.toml — не число"),
};

// Символы линкера без данных: важен только адрес (= значение символа), читать
// по нему нельзя — как и у `from_linkerfile` в `embassy-boot`. Взятие адреса
// (`&raw const`) чтения не делает.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
unsafe extern "C" {
    static __bootloader_state_start: u8;
    static __bootloader_state_end: u8;
    static __bootloader_active_start: u8;
    static __bootloader_active_end: u8;
    static __bootloader_dfu_start: u8;
    static __bootloader_dfu_end: u8;
}

/// Отпечаток раскладки этой прошивки ([`ports::layout_fingerprint`]) — по
/// адресам символов `memory.x` в порядке [`ports::LAYOUT_SYMBOLS`]. Тот же
/// отпечаток `cargo xtask build` считает по значениям символов в ELF и кладёт в
/// хвост образа; узел OTA сверяет их перед пометкой образа к обмену.
pub(crate) fn fingerprint() -> u32 {
    let bounds = [
        &raw const __bootloader_state_start,
        &raw const __bootloader_state_end,
        &raw const __bootloader_active_start,
        &raw const __bootloader_active_end,
        &raw const __bootloader_dfu_start,
        &raw const __bootloader_dfu_end,
    ]
    // Адрес на 32-битной цели — ровно `u32`: отказа здесь не бывает.
    .map(|symbol| u32::try_from(symbol.addr()).expect("адрес на 32-битной цели"));
    ports::layout_fingerprint(bounds)
}
{%- endif %}
{%- if config_store == "flash" or config_store == "external" %}

/// Начало раздела настроек: смещение {% if config_store == "flash" %}от базы flash (так его ждёт
/// `embassy_stm32::flash::Flash`){% else %}от начала внешней флеши{% endif %}.
pub(crate) const CONFIG_START: u32 = match u32::from_str_radix(env!("CONFIG_START"), 10) {
    Ok(start) => start,
    Err(_) => panic!("CONFIG_START в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Конец раздела настроек — см. [`CONFIG_START`].
pub(crate) const CONFIG_END: u32 = match u32::from_str_radix(env!("CONFIG_END"), 10) {
    Ok(end) => end,
    Err(_) => panic!("CONFIG_END в [env] crates-cross/.cargo/config.toml — не число"),
};
{%- endif %}
