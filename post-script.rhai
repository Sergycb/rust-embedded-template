// Пост-хук: обновить оба Cargo.lock под свежие версии зависимостей и
// напечатать, что делать дальше.
//
// `cargo update` намеренно НЕ фатален. Хук, завершившийся ошибкой, обрывает
// генерацию целиком: cargo-generate не оставляет ни каталога проекта, ни
// внятного объяснения — только трейс rhai. А поводов упасть у него хватает и
// без ошибок в шаблоне: нет сети, нет доступа к приватному репозиторию
// git-зависимостей, ломающее изменение в них. Проект с чуть отставшим, но
// закоммиченным Cargo.lock несравнимо полезнее отсутствия проекта.
let updated = true;
try {
	system::command("cargo", ["update"]);
	system::command("cargo", ["update", "--manifest-path", "cross/Cargo.toml"]);
} catch (err) {
	updated = false;
	print("");
	print("cargo update не отработал: " + err);
	print("Проект создан с Cargo.lock из шаблона: версии зафиксированы, но не обновлены.");
	print("Повторить вручную: cargo update && cargo update --manifest-path cross/Cargo.toml");
}

// Итог генерации: значения выставил pre-хук (chip-select.rhai), здесь они уже
// известны. Без этой сводки первое, что видит пользователь в новом проекте, —
// три строки main.rs и четыре файла doc-комментариев с `ignore`-примерами.
let chip = variable::get("chip");
let target = variable::get("target");
let ota = variable::get("ota");

print("");
if updated {
	print("Проект под " + chip + " (" + target + "), зависимости обновлены.");
} else {
	print("Проект под " + chip + " (" + target + ").");
}
if ota == "true" {
	print("Раскладка памяти в cross/app/memory.x и cross/boot/memory.x посчитана по секторам чипа.");
} else {
	print("Без OTA: один образ на весь flash, cross/boot в проект не вошёл (причина — выше).");
}
print("");
print("Дальше:");
print("  cargo xtask setup       # rustup target + probe-rs, flip-link, nextest");
print("  cargo xtask lint        # проверить, что всё на месте");
print("  cargo xtask flash       # прошить плату");
print("");
print("Что заполнить под свою плату: cross/bsp/src/resources.rs (распиновка),");
print("cross/app/src/task_orchestration.rs (граф задач), domain/ (логика).");
