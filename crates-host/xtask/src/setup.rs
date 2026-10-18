//! `cargo xtask setup` — довести пустую машину до рабочего состояния.

use xshell::cmd;

use crate::project::TARGET;

/// Ставит всё, без чего команды проекта падают с невнятной ошибкой линкера или
/// `no such command`: сборку, тесты на плате и проверки из CI.
pub fn setup(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // Компоненты — не «на всякий случай»: профиль rustup бывает minimal (так
    // устроены и оба CI-образа), и тогда `cargo xtask lint` сразу после setup
    // падал бы на `cargo fmt`/`cargo clippy` с "no such command".
    // `llvm-tools-preview` — для `cargo llvm-cov`: без него тот отказывается
    // считать покрытие и сам предлагает поставить компонент.
    cmd!(sh, "rustup component add rustfmt clippy llvm-tools-preview").run()?;
    cmd!(sh, "rustup target add {TARGET}").run()?;

    // Проверяем именно бинарники, а не `cargo <подкоманда>`: `cargo nextest
    // --version` на машине без nextest запускается успешно — это сам `cargo`
    // печатает «no such command», — и установка молча пропускалась бы.
    // `cargo-flash` — отдельный бинарь того же пакета, им этапы `test target`
    // и `test host-target` заливают образы перед прогоном.
    install_if_missing(sh, &["probe-rs", "cargo-flash"], "probe-rs-tools")?;
    install_if_missing(sh, &["flip-link"], "flip-link")?;
    install_if_missing(sh, &["cargo-nextest"], "cargo-nextest")?;
    // Дальше — то, что гоняет CI, чтобы его шаги воспроизводились локально:
    // лицензии и уязвимости (`cargo deny check`), сочетания фич (`cargo hack`),
    // покрытие (`cargo llvm-cov`) и разбор изменившихся снапшотов `insta`.
    install_if_missing(sh, &["cargo-deny"], "cargo-deny")?;
    install_if_missing(sh, &["cargo-hack"], "cargo-hack")?;
    install_if_missing(sh, &["cargo-llvm-cov"], "cargo-llvm-cov")?;
    install_if_missing(sh, &["cargo-insta"], "cargo-insta")?;
    // Текстовые проверки, которые тоже гоняет CI: неиспользуемые зависимости
    // (`cargo machete`), опечатки (`typos`) и формат манифестов (`taplo fmt
    // --check`). У двух последних пакет и бинарник называются по-разному.
    install_if_missing(sh, &["cargo-machete"], "cargo-machete")?;
    install_if_missing(sh, &["typos"], "typos-cli")?;
    install_if_missing(sh, &["taplo"], "taplo-cli")?;
    // Вручную, в CI их нет: мутационное тестирование (`cargo mutants`) идёт
    // часами, а `cargo bloat` отвечает на вопрос «из чего состоит образ», а не
    // «прошла ли проверка».
    install_if_missing(sh, &["cargo-mutants"], "cargo-mutants")?;
    install_if_missing(sh, &["cargo-bloat"], "cargo-bloat")?;
    Ok(())
}

/// Ставит пакет, только если его бинарника ещё нет.
///
/// Голый `cargo install` для этого не годится: пропускает он лишь ту же
/// версию, а при установленной постарше падает с «binary `probe-rs.exe`
/// already exists in destination, add --force» — то есть `cargo xtask setup`
/// начинает падать ровно у тех, у кого всё уже стоит. Ставить с `--force`
/// тоже не вариант: тогда каждый запуск пересобирает probe-rs-tools из
/// исходников. Обновляются инструменты отдельно и осознанно
/// (`cargo install <пакет> --force`), задача setup — довести пустую машину до
/// рабочего состояния.
///
/// Признак «установлен» — то, что процесс запустился, а не его код возврата:
/// `flip-link --version` без аргументов линкера отвечает справкой lld и
/// ненулевым кодом, хотя сам на месте. Отсутствующая программа даёт `Err`
/// ещё до запуска. По `cargo install --list` проверять нельзя: инструмент
/// могли поставить `cargo binstall`'ом или пакетным менеджером, и тогда его
/// в списке нет, а бинарник есть.
fn install_if_missing(
    sh: &xshell::Shell,
    binaries: &[&str],
    package: &str,
) -> Result<(), anyhow::Error> {
    let present = |binary: &&str| {
        cmd!(sh, "{binary} --version")
            .quiet()
            .ignore_status()
            .output()
            .is_ok()
    };
    if binaries.iter().all(present) {
        println!("{package}: уже установлен, пропускаем");
        return Ok(());
    }
    cmd!(sh, "cargo install {package} --locked").run()?;
    // Постусловие: успешный `cargo install` ещё не значит, что бинарь виден. Он
    // лежит в `~/.cargo/bin`, которого может не быть в PATH оболочки, запущенной
    // до правки профиля rustup, — и `setup` с кодом 0 оставил бы все следующие
    // команды без `probe-rs`/`flip-link`/`cargo-nextest`.
    if let Some(missing) = binaries.iter().find(|binary| !present(binary)) {
        anyhow::bail!(
            "`cargo install {package}` прошёл, но `{missing}` по-прежнему не запускается: \
             проверьте, что каталог bin из CARGO_HOME (по умолчанию ~/.cargo/bin) есть в PATH \
             этой оболочки"
        );
    }
    Ok(())
}
