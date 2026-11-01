//! Сценарии `cargo xtask test host-target`: хост проверяет готовое
//! устройство через транспорт и протокол проекта (клиент — `src/lib.rs`).
//!
//! Прошивку, если есть пробник, заливает `xtask` (release) и передаёт сюда
//! через окружение то, что знает только он: чип (`HOST_TARGET_CHIP`) и
//! прошитую версию (`HOST_TARGET_EXPECT_VERSION` — с OTA и только если шил сам).
// {%- if ota == "true" and has_protobuf == "true" %}
//! Для OTA — образы и их подписи (`HOST_TARGET_*_IMAGE`, `*_SIG`) и размер
//! `ACTIVE` для сроков ожидания.
// {%- endif %}
//! Пробника нет — сценарии идут против того, что уже прошито.
//!
//! Идут строго по одному (`--test-threads 1` в `xtask`): канал к устройству
//! один, и два сценария разом мешали бы друг другу. Каждый начинает с того,
//! что устройство на связи, и оставляет его на рабочем образе.
// {%- if has_protobuf != "true" %}
//!
//! Сценариев здесь нет: они написаны под protobuf, а в проекте
// {%- if has_modbus == "true" %}
//! Modbus — его сценарии придут следующей итерацией шаблона
// {%- elsif has_canopen == "true" %}
//! CANopen — его сценарии отложены
// {%- else %}
//! свой протокол — тесты под него пишет проект, поверх своего канала
// {%- endif %}
//! (куда они встанут — `src/lib.rs`).
// {%- endif %}
// {%- if has_protobuf == "true" %}

use std::env;
// {%- if ota == "true" %}
use std::fs;
// {%- endif %}
use std::time::Duration;

use host_target_tests::Device;
// {%- if ota == "true" %}
use host_target_tests::{Info, slow_timeout};

/// Версия тестового образа `app-unconfirmed` (`u32::MAX`,
/// `crates-cross/app/src/version.rs`).
const MAX_VERSION: &str = "255.255.65535";

/// Подпись, которой заведомо нет: устройство подпись не проверяет (только
/// её наличие), и отказ той же версии приходит раньше; пропусти его
/// устройство по ошибке — такой образ не пустит bootloader.
const NOT_A_SIGNATURE: [u8; 64] = [0xA5; 64];
// {%- endif %}

/// Сколько ждать устройство, не дольше: старт после прошивки, перечисление
/// USB, подъём сети.
const BOOT_WAIT: Duration = Duration::from_secs(30);

/// Устройство на связи и называет себя: чип — тот, под который собран
/// проект, версия — та, что прошили (если прошивали здесь).
#[test]
fn info_answers() {
    let Some(mut device) = Device::from_env() else {
        return;
    };
    // Ждём, а не спрашиваем раз: сценарий бывает первым сразу после прошивки,
    // когда порт USB CDC ещё не появился, а сеть не поднялась.
    let info = device
        .wait_back(BOOT_WAIT)
        .unwrap_or_else(|why| panic!("устройство не ответило на Info: {why}"));
    let chip = required("HOST_TARGET_CHIP");
    assert!(
        info.chip.eq_ignore_ascii_case(&chip),
        "устройство — {}, а проект собран под {chip}",
        info.chip
    );
    if let Ok(expected) = env::var("HOST_TARGET_EXPECT_VERSION") {
        assert_eq!(
            release(&info.version),
            expected,
            "на устройстве не та версия, что прошили только что"
        );
    }
}
// {%- if ota == "true" %}

/// `Reboot` отвечает до сброса, устройство возвращается в срок, и причина
/// сброса — программная: сбросила его команда, а не сторож или питание.
#[test]
fn reboot_comes_back() {
    let Some(mut device) = Device::from_env() else {
        return;
    };
    settle(&mut device);
    assert_eq!(device.reboot().expect("Reboot"), Ok(()));
    let info = device
        .wait_back(slow_timeout())
        .unwrap_or_else(|why| panic!("устройство не вернулось после Reboot: {why}"));
    assert_eq!(info.reset, "software", "причина сброса после Reboot");
}

/// `Start` без подписи отвергается до стирания раздела — образ, в который
/// устройство откатывается, цел, — и работает прежняя прошивка.
#[test]
fn unsigned_start_is_rejected() {
    let Some(mut device) = Device::from_env() else {
        return;
    };
    let before = settle(&mut device);
    let image = file("HOST_TARGET_UNCONFIRMED_IMAGE");
    assert_eq!(
        device
            .start(image.len(), &[], slow_timeout())
            .expect("Start"),
        Err("Signature".to_owned()),
        "Start без подписи"
    );
    assert_eq!(device.info().expect("Info").version, before.version);
}

/// Образ под чужим ключом приложение принимает — подпись проверяет не оно, а
/// bootloader, — и bootloader не пускает его: после сброса работает прежняя
/// прошивка.
#[test]
fn foreign_signature_is_not_swapped() {
    let Some(mut device) = Device::from_env() else {
        return;
    };
    let before = settle(&mut device);
    let image = file("HOST_TARGET_UNCONFIRMED_IMAGE");
    let signature = file("HOST_TARGET_FOREIGN_SIG");
    assert_eq!(
        device
            .upload(&image, &signature, slow_timeout())
            .expect("загрузка образа"),
        Ok(()),
        "приложение подпись не проверяет — Finish обязан пройти"
    );
    assert_eq!(device.reboot().expect("Reboot"), Ok(()));
    let after = back(&mut device);
    if after.version == MAX_VERSION {
        // Встал — откатываем, чтобы оставить устройство на рабочем образе.
        if device.reboot().is_ok_and(|outcome| outcome.is_ok()) {
            device.wait_back(slow_timeout()).ok();
        }
        panic!("bootloader пустил образ под ЧУЖИМ ключом: подпись OTA ничего не защищает");
    }
    assert_eq!(after.version, before.version);
}

/// Главный тест обмена: тестовый образ `app-unconfirmed` встаёт после
/// сброса (обмен разделов), но себя не подтверждает — и следующий сброс
/// возвращает прежнюю прошивку (откат). Без перепрошивки: устройство
/// остаётся на рабочем образе.
#[test]
fn unconfirmed_image_is_swapped_and_rolled_back() {
    let Some(mut device) = Device::from_env() else {
        return;
    };
    let Some(signature) = env::var_os("HOST_TARGET_UNCONFIRMED_SIG") else {
        eprintln!("закрытого ключа нет — тестовый образ не подписан, обмен не проверить");
        return;
    };
    let before = settle(&mut device);
    let image = file("HOST_TARGET_UNCONFIRMED_IMAGE");
    let signature = fs::read(&signature).expect("прочитать подпись тестового образа");
    assert_eq!(
        device
            .upload(&image, &signature, slow_timeout())
            .expect("загрузка образа"),
        Ok(())
    );
    assert_eq!(device.reboot().expect("Reboot"), Ok(()));
    let swapped = back(&mut device);
    assert_eq!(
        swapped.version, MAX_VERSION,
        "после сброса работает не тестовый образ — обмена не было. Если плату прошивали не \
         здесь (пробника не было), в её bootloader'е может быть другой открытый ключ, и он \
         отвергает образы этого проекта"
    );
    assert_eq!(device.reboot().expect("Reboot"), Ok(()));
    let reverted = back(&mut device);
    assert_eq!(
        reverted.version, before.version,
        "неподтверждённый образ не откатился: неработающая прошивка, проехавшая по OTA, \
         осталась бы на устройстве"
    );
}

/// Образ той же версии отвергается на `Finish` — защита от отката.
#[test]
fn same_version_is_rejected() {
    let Some(mut device) = Device::from_env() else {
        return;
    };
    let running = settle(&mut device);
    let image = file("HOST_TARGET_IMAGE");
    let tail = image.len() - 4;
    let version = u32::from_le_bytes(image[tail..].try_into().expect("версия в хвосте образа"));
    let Some(running_version) = pack(release(&running.version)) else {
        panic!(
            "версия устройства {} — не major.minor.patch",
            running.version
        );
    };
    if running_version < version {
        eprintln!(
            "на устройстве {} — старше собранного образа, отказ той же версии здесь не проверить",
            running.version
        );
        return;
    }
    assert_eq!(
        device
            .upload(&image, &NOT_A_SIGNATURE, slow_timeout())
            .expect("загрузка образа"),
        Err("Rollback".to_owned()),
        "образ не новее работающего обязан отвергаться (`Layout` вместо `Rollback` — \
         устройство прошито под другую раскладку memory.x)"
    );
}

/// Устройство на связи и на рабочем образе: остался тестовый (прерванный
/// прогон) — сброс его откатывает.
fn settle(device: &mut Device) -> Info {
    let info = back(device);
    if info.version != MAX_VERSION {
        return info;
    }
    eprintln!("на устройстве тестовый образ от прерванного прогона — откатываем сбросом");
    assert_eq!(device.reboot().expect("Reboot"), Ok(()));
    back(device)
}

/// Ответ после сброса — или провал с причиной.
fn back(device: &mut Device) -> Info {
    device
        .wait_back(slow_timeout())
        .unwrap_or_else(|why| panic!("устройство не на связи: {why}"))
}

/// Байты файла, путь к которому `xtask` положил в `var`.
fn file(var: &str) -> Vec<u8> {
    let path = required(var);
    fs::read(&path).unwrap_or_else(|err| panic!("{var}={path}: {err}"))
}

/// `major.minor.patch` — в число, как его сворачивает прошивка
/// (`domain::ota::version::pack`).
fn pack(version: &str) -> Option<u32> {
    let mut parts = version.split('.');
    let major: u8 = parts.next()?.parse().ok()?;
    let minor: u8 = parts.next()?.parse().ok()?;
    let patch: u16 = parts.next()?.parse().ok()?;
    parts
        .next()
        .is_none()
        .then_some((u32::from(major) << 24) | (u32::from(minor) << 16) | u32::from(patch))
}
// {%- endif %}

/// Версия без пререлизного суффикса: прошивка сравнивает и называет
/// `major.minor.patch` (README, «Защита от отката»).
fn release(version: &str) -> &str {
    version.split(['-', '+']).next().unwrap_or(version)
}

/// Переменная, которую задаёт `xtask`.
fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!("{name} не задана: этот этап запускается через `cargo xtask test host-target`")
    })
}
// {%- endif %}
