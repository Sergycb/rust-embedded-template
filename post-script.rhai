// Выполнить команду cargo update в только что созданном каталоге проекта
system::command("cargo", ["update"]);
system::command("cd", ["cross"]);
system::command("cargo", ["update"]);