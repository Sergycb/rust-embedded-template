// Пост-хук: сводка по сгенерированному проекту.
//
// `cargo update` здесь был и намеренно убран. Он занимал больше половины
// времени генерации (два вызова, каждый резолвит свой воркспейс и лезет за
// индексом crates.io и за git-зависимостями; на холодном кеше — минуты
// молчания, выглядящие как зависание), а главное — уводил свежий проект с
// того состояния, которое проверено `template-check`, на текущий `main`
// приватных git-зависимостей. Ровно так однажды и сломалась генерация: у
// `fsm` исчезла фича `macro`, и проекты перестали создаваться, хотя все
// локальные проверки оставались зелёными.
//
// Lock-файлов проект из шаблона не получает вовсе (см. ниже) — их считает
// первая сборка проекта. Обновление версий потом — дело dependabot/renovate в
// самом проекте или явного `cargo update`, когда этого захотят.
//
// Побочный эффект: хук больше не выполняет системных команд, поэтому
// `cargo generate` не спрашивает разрешения на них и не требует
// `--allow-commands` в неинтерактивном режиме.

// Значения выставил pre-хук (chip-select.rhai), здесь они уже известны. Без
// этой сводки первое, что видит пользователь в новом проекте, — три строки
// main.rs и четыре файла doc-комментариев с `ignore`-примерами.
let chip = variable::get("chip");
let target = variable::get("target");
let ota = variable::get("ota");
let config = variable::get("config");

// Версия и ключ подписи живут в приложении и нужны только с подписью. Здесь,
// а не в pre-хуке: окончательное `signed` pre-хук выставляет в трёх ветках
// раскладки, а к этому моменту оно уже одно.
if variable::get("signed") != "true" {
	file::delete("crates-cross/app/src/image.rs");
}

print("");
print("Проект под " + chip + " (" + target + ").");
if ota == "true" {
	print("Раскладка памяти в crates-cross/app/memory.x и crates-cross/boot/memory.x посчитана по секторам чипа.");
} else {
	print("Без OTA: один образ на весь flash, crates-cross/boot в проект не вошёл (причина — выше).");
}
if config == "true" {
	let store = variable::get("config_store");
	if store == "eeprom" {
		print("Настройки: внутренняя EEPROM чипа, crates-cross/bsp/src/config.rs, Board::settings.");
	} else if store == "external" {
		print("Настройки: внешняя флеш — подставьте драйвер в crates-cross/bsp/src/config.rs, Board::settings.");
	} else {
		print("Настройки: раздел CONFIG во flash, crates-cross/bsp/src/config.rs, Board::settings.");
	}
}
if variable::get("graph") == "true" {
	print("Граф задач: узел HEARTBEAT в crates-cross/app/src/graph.rs, его работа — в crates-host/domain/src/heartbeat.rs.");
	let protocol = variable::get("protocol");
	if protocol == "modbus" {
		print("Modbus slave: узел MODBUS поверх " + variable::get("interface").to_upper() + "; карта регистров — crates-host/domain/src/modbus/registers.rs.");
	} else if protocol == "canopen" {
		print("CANopen: узел CANOPEN поверх CAN; словарь объектов — crates-host/canopen/device.toml.");
	} else if protocol == "protobuf" {
		print("protobuf: узел HOST поверх " + variable::get("interface").to_upper() + "; схема — crates-host/proto, обработчик — crates-host/domain/src/host/service.rs.");
	}
	let iface = variable::get("interface");
	if iface == "uart" {
		print("UART — заглушка: подставьте свой USART в crates-cross/bsp/src/serial.rs (cargo xtask pins USART2 --snippet).");
	} else if iface == "usb" {
		let block = if variable::get("usb_kind") == "otg" { "USB_OTG_FS (или USB_OTG_HS)" } else { "USB" };
		print("USB — устройство embassy-usb над заглушкой драйвера: подставьте USB-блок в crates-cross/bsp/src/usb.rs (cargo xtask pins " + block + " --snippet).");
	} else if iface == "tcp" || iface == "udp" {
		print("Сеть — стек embassy-net над заглушкой устройства: подставьте Ethernet в crates-cross/bsp/src/net.rs (cargo xtask pins ETH --snippet).");
	} else if iface == "can" {
		let block = if variable::get("can_kind") == "fdcan" { "FDCAN1" } else { "CAN1" };
		print("CAN — заглушка: подставьте свой контроллер в crates-cross/bsp/src/can.rs (cargo xtask pins " + block + " --snippet).");
	}
} else {
	print("Без графа задач: main заканчивается после инициализации (причина — выше).");
}

// Lock-файлов шаблон не раздаёт: состав зависимостей зависит от ответов выше —
// при `ota=no` уходят `embassy-boot*`, при `config=internal` добавляются
// `sequential-storage` с `embedded-storage-async`, при OTA — `salty`,
// протокол приносит свои крейты. Один шаблонный lock подходил бы ровно одному
// сочетанию, а остальным ломал бы `cargo fetch --locked` в CI. Поэтому их
// считает первая же сборка проекта — и вот об этом надо сказать вслух, иначе
// файлы появятся «сами собой» и рискуют остаться незакоммиченными.
print("");
print("Lock-файлов в проекте пока нет — состав зависимостей зависит от ответов");
print("выше, поэтому их считает первая сборка. Соберите и закоммитьте Cargo.lock,");
print("crates-cross/Cargo.lock и crates-host/chip-info/Cargo.lock, иначе CI будет");
print("резолвить версии заново каждый прогон:");
print("  cargo xtask build && cargo xtask pins");

print("");
print("Дальше:");
print("  cargo xtask setup       # тулчейн, probe-rs, flip-link, cargo-плагины");
print("  cargo xtask lint        # проверить, что всё на месте");
print("  cargo xtask test        # этапы тестирования");
print("");
if variable::get("graph") == "true" {
	print("Что заполнить под свою плату: crates-cross/bsp/src/board.rs (распиновка),");
	print("crates-cross/app/src/graph.rs (граф задач), crates-host/domain/ (логика).");
} else {
	// Файла графа в таком проекте нет вовсе — его удаляет chip-select.rhai
	// вместе с отключением графа, и звать сюда некуда.
	print("Что заполнить под свою плату: crates-cross/bsp/src/board.rs (распиновка),");
	print("crates-host/domain/ (логика).");
}
// Справочник упомянут ровно здесь, а не в списке «Дальше» выше: он нужен не
// на старте, а в тот момент, когда садишься за распиновку и вспоминаешь,
// что распиновку неоткуда взять, кроме даташита.
print("");
print("  cargo xtask pins SPI1   # какие выводы умеют этот блок на " + chip + ", DMA, тактирование");
