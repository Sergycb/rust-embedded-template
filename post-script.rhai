// Выполнить команду cargo update в только что созданном каталоге проекта
shell("cargo update")?;
shell("cd cross")?;
shell("cargo update")?;