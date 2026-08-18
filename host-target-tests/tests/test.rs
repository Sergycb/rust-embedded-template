//! Тесты, где хост управляет уже прошитым устройством
//! (`cargo xtask test host-target`).
//!
//! Третий этап трёхэтапной модели. Отличие от двух других — в том, кто кем
//! командует: `cargo xtask test host` гоняет логику `domain` на хосте без
//! всякого железа, `cargo xtask test target` выполняет код *внутри* МК, а
//! здесь тест — обычная host-программа, которая смотрит на устройство
//! снаружи и общается с ним так же, как это будет делать реальный оператор
//! или соседний сервис.
//!
//! Прошивку заливает `cargo xtask test host-target` (release-профиль,
//! bootloader + приложение) и передаёт сюда два значения через окружение:
//! `HOST_TARGET_CHIP` и `HOST_TARGET_APP_ELF`. Через них, а не константами в
//! коде — путь зависит от профиля, а имя чипа подставляется при генерации в
//! ровно одном месте (`xtask`).
//!
//! Внешних зависимостей у крейта нет намеренно: `probe-rs` для этого этапа и
//! так обязателен (им же прошивают), и вызвать его как процесс дешевле, чем
//! тянуть в проект его библиотечную версию со своим графом зависимостей.
//! Когда у платы появится собственный канал связи (USB CDC, UART-протокол,
//! сеть), настоящие сценарии стоит писать поверх него — этот тест останется
//! проверкой того, что устройство вообще живо.

use std::{
    env,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

/// Первое, что печатает прошивка из шаблона (`cross/app/src/main.rs`).
/// Меняете баннер там — поменяйте и здесь: тест ровно про то, что реальное
/// устройство доходит до этой строки.
const BANNER: &str = "app: starting";

/// С запасом на сброс, подъём RTT и старт bootloader'а — но не бесконечность,
/// иначе упавшая прошивка вешает прогон вместо того, чтобы его провалить.
const TIMEOUT: Duration = Duration::from_secs(15);

#[test]
fn firmware_announces_itself_over_rtt() {
    let chip = required_var("HOST_TARGET_CHIP");
    let elf = required_var("HOST_TARGET_APP_ELF");

    let mut child = Command::new("probe-rs")
        .args(["attach", "--chip", &chip, &elf])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("не удалось запустить probe-rs — установите его: cargo xtask setup");

    let (sender, receiver) = mpsc::channel();
    // Оба потока: probe-rs пишет часть диагностики в stderr, и без неё
    // сообщение о провале было бы «ничего не пришло» вместо реальной причины
    // («No debug probe found», «target not halted» и подобных).
    for stream in [
        child.stdout.take().map(BufReader::new).map(Reader::Out),
        child.stderr.take().map(BufReader::new).map(Reader::Err),
    ]
    .into_iter()
    .flatten()
    {
        let sender = sender.clone();
        thread::spawn(move || stream.forward_lines(&sender));
    }
    // Иначе receiver никогда не увидит Disconnected: собственная копия
    // отправителя удерживала бы канал открытым.
    drop(sender);

    let deadline = Instant::now() + TIMEOUT;
    let mut seen: Vec<String> = Vec::new();
    let mut found = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match receiver.recv_timeout(left) {
            Ok(line) => {
                if line.contains(BANNER) {
                    found = true;
                    break;
                }
                seen.push(line);
            }
            // Disconnected — probe-rs завершился сам, ждать больше нечего.
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }

    if let Err(err) = child.kill() {
        eprintln!("не удалось остановить probe-rs: {err}");
    }
    if let Err(err) = child.wait() {
        eprintln!("не удалось дождаться probe-rs: {err}");
    }

    assert!(
        found,
        "за {} с устройство не напечатало {BANNER:?}. Что пришло от probe-rs:\n{}",
        TIMEOUT.as_secs(),
        if seen.is_empty() {
            "(ничего)".to_owned()
        } else {
            seen.join("\n")
        },
    );
}

fn required_var(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} не задана: этот этап запускается через `cargo xtask test host-target`, \
             который сам прошивает плату и передаёт сюда чип и путь к ELF"
        )
    })
}

enum Reader {
    Out(BufReader<std::process::ChildStdout>),
    Err(BufReader<std::process::ChildStderr>),
}

impl Reader {
    fn forward_lines(self, sender: &mpsc::Sender<String>) {
        match self {
            Reader::Out(reader) => forward(reader.lines(), sender),
            Reader::Err(reader) => forward(reader.lines(), sender),
        }
    }
}

fn forward<L: Iterator<Item = std::io::Result<String>>>(lines: L, sender: &mpsc::Sender<String>) {
    for line in lines.map_while(Result::ok) {
        // Ошибка отправки означает, что тест уже закончил ждать.
        if sender.send(line).is_err() {
            return;
        }
    }
}
