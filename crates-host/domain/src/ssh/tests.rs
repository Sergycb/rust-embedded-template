//! Тесты узла `SSH`: ключ пользователя, команды, `info`, ключ хоста.

use crate::device::{DeviceInfo, PANIC_BRIEF};
use crate::ssh::console::{INFO_TEXT, parse, write_info};
use crate::ssh::{Command, ed25519_public_key};
// {%- if ssh_host_key == "config" %}
use crate::ssh::{HostKey, Stored};
// {%- endif %}

const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl user@host";
// {%- if ssh_host_key == "config" %}

/// Хранилище в памяти: что записано, то и читается.
#[derive(Default)]
struct Memory(Option<Vec<u8>>);

impl ports::SettingsStorage for Memory {
    type Error = ();
    const MAX_VALUE: usize = usize::MAX;

    fn read<'a>(
        &mut self,
        _: u32,
        scratch: &'a mut [u8],
    ) -> impl Future<Output = Result<Option<&'a [u8]>, ()>> {
        // Как у порта: значение длиннее буфера — ошибка, а не обрезка.
        core::future::ready(match &self.0 {
            None => Ok(None),
            Some(v) if v.len() > scratch.len() => Err(()),
            Some(v) => {
                scratch[..v.len()].copy_from_slice(v);
                Ok(Some(&scratch[..v.len()]))
            }
        })
    }

    fn write(&mut self, _: u32, value: &[u8]) -> impl Future<Output = Result<(), ()>> {
        self.0 = Some(value.to_vec());
        core::future::ready(Ok(()))
    }
}
// {%- endif %}

#[test]
fn an_openssh_line_gives_the_raw_key() {
    let key = ed25519_public_key(KEY).expect("ключ разбирается");
    assert_eq!(key[..4], [0xe3, 0x2a, 0xaa, 0x79]);
    assert_eq!(key[28..], [0x7d, 0x18, 0xa2, 0x65]);
    // Комментарий необязателен.
    assert_eq!(
        ed25519_public_key(&KEY[..KEY.len() - " user@host".len()]),
        Some(key)
    );
}

#[test]
fn anything_else_is_not_a_key() {
    for line in [
        "",
        "ssh-ed25519",
        "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJ!",
    ] {
        assert_eq!(ed25519_public_key(line), None, "{line}");
    }
}

#[test]
fn commands_are_whole_words() {
    assert_eq!(parse(""), Command::Empty);
    assert_eq!(parse("  help "), Command::Help);
    assert_eq!(parse("info"), Command::Info);
    assert_eq!(parse("exit"), Command::Exit);
    assert_eq!(parse("info now"), Command::Unknown);
    assert_eq!(parse("helpme"), Command::Unknown);
    // {%- if ota == "true" %}
    assert_eq!(parse("reboot"), Command::Reboot { confirmed: false });
    assert_eq!(parse("reboot yes"), Command::Reboot { confirmed: true });
    assert_eq!(parse("reboot yes please"), Command::Unknown);
    // {%- endif %}
}

#[test]
fn info_fits_the_reply_buffer() {
    let info = DeviceInfo {
        version: "65535.65535.65535-rc.65535",
        commit: "0123456789ab",
        chip: "STM32H7R7L8HXH",
        uid: [0xff; crate::device::UID_BYTES],
        flash_size: u32::MAX,
        sys_hz: Some(u32::MAX),
        reset: crate::device::ResetCause::IndependentWatchdog,
        last_panic: Some(String::leak("ж".repeat(PANIC_BRIEF / 2))),
    };
    let mut out = heapless::String::<INFO_TEXT>::new();
    write_info(
        &mut out,
        &info,
        [0xff; 6],
        Some(embassy_net::Ipv4Address::BROADCAST),
        u64::MAX,
    )
    .unwrap();
    assert!(out.starts_with("version 65535"), "{out}");
    assert!(
        out.contains("\nreset independent_watchdog\nlast panic ж"),
        "{out}"
    );
}

/// Место и текст паники — одна строка `last panic …`, а не две.
#[test]
fn the_panic_stays_on_its_line() {
    let info = DeviceInfo {
        version: "0.1.0",
        commit: "abc",
        chip: "STM32G071RB",
        uid: [0; crate::device::UID_BYTES],
        flash_size: 0,
        sys_hz: None,
        reset: crate::device::ResetCause::Software,
        last_panic: Some("dhcp.rs:12:5:\nlease lost"),
    };
    let mut out = heapless::String::<INFO_TEXT>::new();
    write_info(&mut out, &info, [0; 6], None, 0).unwrap();
    assert!(
        out.ends_with("\nlast panic dhcp.rs:12:5: lease lost\n"),
        "{out}"
    );
}
// {%- if ssh_host_key == "config" %}

/// Первый старт создаёт ключ и записывает, следующий читает тот же.
#[tokio::test]
async fn the_stored_key_survives_a_restart() {
    let mut stored = Stored(Memory::default());
    let first = stored.seed().await;
    assert_ne!(first, [0; 32]);
    assert_eq!(stored.0.0.as_deref(), Some(&first[..]));
    assert_eq!(Stored(Memory(stored.0.0.clone())).seed().await, first);
}

/// Ключ не той длины заменяется новым и записывается; раздел, который не
/// прочитался (здесь — значение длиннее буфера), не перезаписывается:
/// сбой мог быть временным.
#[tokio::test]
async fn only_a_missing_key_is_overwritten() {
    let mut short = Stored(Memory(Some(vec![1; 16])));
    let seed = short.seed().await;
    assert_eq!(short.0.0.as_deref(), Some(&seed[..]));

    let unreadable = vec![2; 64];
    let mut broken = Stored(Memory(Some(unreadable.clone())));
    broken.seed().await;
    assert_eq!(broken.0.0, Some(unreadable));
}
// {%- endif %}
