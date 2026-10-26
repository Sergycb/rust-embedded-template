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
	file::delete("crates-cross/app/src/version.rs");
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
		print("Настройки: внутренняя EEPROM чипа (crates-cross/bsp/src/flash.rs, поле Board::config); хранилище над ней (adapters::settings) собирает узел-потребитель.");
	} else if store == "external" {
		print("Настройки: внешняя флеш — подставьте шину драйвера в crates-cross/bsp/src/external.rs (поле Board::config); хранилище над ней (adapters::settings) собирает узел-потребитель.");
	} else {
		print("Настройки: раздел CONFIG во flash (crates-cross/bsp/src/flash.rs, поле Board::config); хранилище над ним (adapters::settings) собирает узел-потребитель.");
	}
}
if variable::get("graph") == "true" {
	print("Граф задач: узел HEARTBEAT в crates-cross/app/src/graph.rs, его работа — в crates-host/domain/src/heartbeat.rs.");
} else {
	print("Без графа задач: узлы спавнит tasks::spawn_all (crates-cross/app/src/tasks/, модуль на задачу) — без сторожа и restart: (причина — выше).");
}
{
	// По транспорту на строку (`вид:метка:блок:протокол:имя` из `links`): узел
	// протокола и заглушка, которую подставить. Сеть — один стек (узел NET) и один
	// блок Ethernet на все сокеты, поэтому о нём — одной строкой после цикла.
	let links = variable::get("links");
	let net_block = "";
	let blocks = "";
	if links != "" {
		for l in links.split(",") {
			let f = l.split(":");
			let kind = f[0];
			let block = f[2];
			// `tcp` и `udp` делят один ETH — в списке блоков он один раз.
			if !(", " + blocks + ", ").contains(", " + block + ", ") {
				blocks += (if blocks == "" { "" } else { ", " }) + block;
			}
			let proto = f[3];
			let name = f[4];
			let node = if proto == "modbus" { "MODBUS_" } else if proto == "protobuf" { "HOST_" } else if proto == "canopen" { "CANOPEN_" } else { "" };
			let nodes = [];
			if node != "" { nodes.push("узел " + node + name.to_upper()); }
			if kind == "usb" { nodes.push("фоновый узел USB_" + name.to_upper()); }
			let line = "Транспорт " + name + " (" + kind.to_upper() + ", " + block + ", "
				+ (if proto == "none" { "без протокола" } else { proto }) + ")";
			if nodes.len() > 0 { line += ": " + nodes[0]; }
			if nodes.len() > 1 { line += ", " + nodes[1]; }
			if kind == "tcp" || kind == "udp" {
				net_block = block;
				print(line + ".");
			} else {
				let file = if kind == "uart" { "serial" } else { kind };
				print(line + "; заглушка — crates-cross/bsp/src/" + file + ".rs (cargo xtask pins " + block + " --snippet).");
			}
		}
	}
	if net_block != "" {
		print("Сеть: фоновый узел NET — стек embassy-net над заглушкой устройства, оба в crates-cross/bsp/src/net.rs (cargo xtask pins " + net_block + " --snippet).");
	}
	if blocks != "" {
		print("Блоки интерфейсов назначены по умолчанию: " + blocks + ". Сменить блок и задать выводы своей платы — crates-cross/bsp/src/resources.rs (cargo xtask pins <БЛОК> --snippet печатает готовую группу); имена транспортов (поле Board, модули bsp и app, имена узлов) остаются от блока по умолчанию.");
	}
	if variable::get("has_modbus") == "true" {
		print("Modbus: карта регистров — crates-host/domain/src/modbus/registers.rs; одна на все узлы, тип подставляет crates-cross/app/src/modbus.rs (псевдоним Map, строится Map::default()).");
	}
	if variable::get("has_canopen") == "true" {
		print("CANopen: словарь объектов — crates-host/canopen/device.toml.");
	}
	if variable::get("has_protobuf") == "true" {
		print("protobuf: схема — crates-host/proto, обработчик — crates-host/domain/src/protobuf/service.rs.");
	}
}

// Lock-файлов шаблон не раздаёт: состав зависимостей зависит от ответов выше —
// при `ota=no` уходят `embassy-boot*`, при `config=yes` добавляются
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
print("  cargo xtask test host   # тесты domain на хосте; этапы с платой — README");
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
