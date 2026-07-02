// Выполнить команду cargo update в только что созданном каталоге проекта
system::command("cargo", ["update"]);
system::command("cargo", ["update", "--manifest-path", "cross/Cargo.toml"]);