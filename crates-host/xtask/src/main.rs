mod firmware;
mod flash;
mod image;
mod lint;
mod memory;
mod panic_info;
mod pins;
mod project;
mod secrets;
mod setup;
mod signing;
mod size;
mod test;

use std::env;

use project::{CHIP, signing_enabled};

fn main() -> Result<(), anyhow::Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let args = args.iter().map(|s| &**s).collect::<Vec<_>>();
    let sh = xshell::Shell::new()?;

    match &args[..] {
        ["setup"] => setup::setup(&sh),
        ["test", stage @ ..] => test::test(&sh, stage),
        ["build"] => firmware::build(&sh),
        ["lint"] => lint::lint_host(&sh),
        ["lint", "cross"] => lint::lint_cross(&sh),
        // Аргументы уходят в chip-info как есть (блок, вывод, `--snippet`,
        // `--check`) — разбирает их он, здесь незачем знать его ключи.
        ["pins", args @ ..] => pins::pins(&sh, args),
        ["panic-info"] => panic_info::panic_info(&sh),
        // Без аргументов — просто справка, это не ошибка. А вот непонятая
        // команда завершается ненулевым кодом: иначе опечатка в CI-шаге или в
        // задаче IDE даёт зелёный прогон, в котором ничего не выполнилось.
        [] => {
            usage();
            Ok(())
        }
        other => {
            usage();
            anyhow::bail!("неизвестная команда: cargo xtask {}", other.join(" "))
        }
    }
}

fn usage() {
    println!(
        "USAGE cargo xtask setup                      # тулчейн, probe-rs, flip-link, cargo-плагины"
    );
    println!(
        "      cargo xtask build                      # debug + release, app.bin, размер, подпись"
    );
    println!("      cargo xtask lint [cross]");
    println!("      cargo xtask test [host|target|host-target|all]");
    println!("      cargo xtask pins [БЛОК|ПИН]            # справочник по чипу: SPI1, PA9");
    println!(
        "      cargo xtask pins БЛОК --snippet        # заготовка bind_interrupts!/assign_resources!"
    );
    println!(
        "      cargo xtask pins --check               # не занят ли отладочный порт в resources.rs"
    );
    println!(
        "      cargo xtask panic-info                 # причина последнего падения с платы, с разбором"
    );
    if signing_enabled() {
        println!();
        println!("Отдельных команд для подписи нет: `build` сам создаёт ключевую пару при первой");
        println!("сборке, кладёт открытый ключ в прошивку и подписывает release-образ.");
    }
    println!();
    println!("Всё остальное про плату — напрямую через probe-rs:");
    println!("      probe-rs attach --chip {CHIP} <elf>   # defmt-лог без перепрошивки");
    println!("      probe-rs reset  --chip {CHIP}");
    println!("      probe-rs erase  --chip {CHIP} --connect-under-reset");
}
