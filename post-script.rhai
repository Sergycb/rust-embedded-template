// Пост-хук: удаляет crates-cross/app/src/version.rs без OTA и печатает
// сводку по сгенерированному проекту.
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

// --- Пароль консоли -------------------------------------------------------
//
// Пароль — имя проекта плюс `00`; в прошивку уходит только SHA-256(соль ‖
// пароль), файлов `console-password.*` в проекте нет. SHA-256 ниже — на Rhai,
// чтобы хэш считался здесь, при генерации, и не зависел от build.rs/xtask.
// Для шаблона/стенда этого достаточно; для production пароль надо сменить.

fn sha256_rotr(x, n) {
    ((x >> n) | (x << (32 - n))) & 0xffffffff
}

fn sha256_bytes(text) {
    let msg = blob();
    msg += text;

    let bits = msg.len() * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    for i in 0..8 {
        msg.push((bits >> (56 - i * 8)) & 0xff);
    }

    let k = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let h = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    for chunk in 0..(msg.len() / 64) {
        let w = [];
        for i in 0..16 {
            let j = chunk * 64 + i * 4;
            let b0 = msg[j] << 24;
            let b1 = msg[j + 1] << 16;
            let b2 = msg[j + 2] << 8;
            let b3 = msg[j + 3];
            w.push(b0 | b1 | b2 | b3);
        }
        for i in 16..64 {
            let x = w[i - 15];
            let x7 = sha256_rotr(x, 7);
            let x18 = sha256_rotr(x, 18);
            let x3 = x >> 3;
            let s0 = x7 ^ x18 ^ x3;
            let y = w[i - 2];
            let y17 = sha256_rotr(y, 17);
            let y19 = sha256_rotr(y, 19);
            let y10 = y >> 10;
            let s1 = y17 ^ y19 ^ y10;
            let a = w[i - 16] + s0;
            let b = a + w[i - 7];
            let c = b + s1;
            w.push(c & 0xffffffff);
        }

        let a = h[0];
        let b = h[1];
        let c = h[2];
        let d = h[3];
        let e = h[4];
        let f = h[5];
        let g = h[6];
        let hh = h[7];

        for i in 0..64 {
            let e6 = sha256_rotr(e, 6);
            let e11 = sha256_rotr(e, 11);
            let e25 = sha256_rotr(e, 25);
            let s1 = e6 ^ e11 ^ e25;
            let e_not = e ^ 0xffffffff;
            let ch1 = e & f;
            let ch2 = e_not & g;
            let ch = ch1 ^ ch2;
            let t1a = hh + s1;
            let t1b = t1a + ch;
            let t1c = t1b + k[i];
            let t1d = t1c + w[i];
            let t1 = t1d & 0xffffffff;
            let a2 = sha256_rotr(a, 2);
            let a13 = sha256_rotr(a, 13);
            let a22 = sha256_rotr(a, 22);
            let s0 = a2 ^ a13 ^ a22;
            let maj1 = a & b;
            let maj2 = a & c;
            let maj3 = b & c;
            let maj = maj1 ^ maj2 ^ maj3;
            let t2 = (s0 + maj) & 0xffffffff;

            hh = g;
            g = f;
            f = e;
            e = (d + t1) & 0xffffffff;
            d = c;
            c = b;
            b = a;
            a = (t1 + t2) & 0xffffffff;
        }

        h[0] = (h[0] + a) & 0xffffffff;
        h[1] = (h[1] + b) & 0xffffffff;
        h[2] = (h[2] + c) & 0xffffffff;
        h[3] = (h[3] + d) & 0xffffffff;
        h[4] = (h[4] + e) & 0xffffffff;
        h[5] = (h[5] + f) & 0xffffffff;
        h[6] = (h[6] + g) & 0xffffffff;
        h[7] = (h[7] + hh) & 0xffffffff;
    }

    let out = [];
    for word in h {
        for i in 0..4 {
            let shift = 24 - i * 8;
            let byte = word >> shift;
            out.push(byte & 0xff);
        }
    }
    out
}

fn hex_byte(b) {
    let hex = "0123456789abcdef";
    let out = "";
    out += hex[(b >> 4) & 0xf];
    out += hex[b & 0xf];
    out
}

fn sha256_hex(text) {
    let hex = "0123456789abcdef";
    let out = "";
    for b in sha256_bytes(text) {
        out += hex[(b >> 4) & 0xf];
        out += hex[b & 0xf];
    }
    out
}

// Тест-вектор FIPS 180-4: ошибка в реализации не должна пройти незаметно.
if sha256_hex("abc") != "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" {
    abort("post-script: SHA-256 не прошёл тест-вектор");
}

// Значения выставил pre-хук (chip-select.rhai), здесь они уже известны. Без
// этой сводки пользователь не узнает, что хук решил за него (OTA, граф,
// назначенные блоки транспортов) и что заполнить первым.
let chip = variable::get("chip");
let target = variable::get("target");
let ota = variable::get("ota");
let config = variable::get("config");

// Хэш пароля консоли — константой в исходнике: файлов `console-password.*`
// нет. Пароль — имя проекта плюс `00`; в README это написано явно.
if variable::get("has_console") == "true" {
    let project = variable::get("project-name");
    if project == "" {
        abort("post-script: cargo-generate не передал project-name");
    }
    let salt = "0123456789abcdef";
    let password = project + "00";
    let hash = sha256_bytes(salt + password);

    let source = "// Сгенерировано `post-script.rhai` при создании проекта.\n";
    source += "// Пароль консоли — имя проекта плюс `00`; здесь только соль и хэш.\n";
    source += "pub const PASSWORD_HASH: [u8; 48] = [\n";
    for ch in salt {
        source += "    0x" + hex_byte(ch.to_int()) + ",\n";
    }
    for b in hash {
        source += "    0x" + hex_byte(b) + ",\n";
    }
    source += "];\n";
    file::write("crates-cross/app/src/console_password.rs", source);
    print("Пароль консоли: " + password);
}

// Версия и ключ подписи живут в приложении и нужны только с OTA (подпись —
// всегда вместе с ним). Здесь, а не в pre-хуке: окончательное `ota` pre-хук
// выставляет в нескольких ветках раскладки, а к этому моменту оно уже одно.
if variable::get("ota") != "true" {
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
		print("Настройки: внутренняя EEPROM чипа (crates-cross/bsp/src/flash.rs, поле Board::config); хранилище над ней (domain::settings) строит приложение и отдаёт узлу-потребителю.");
	} else if store == "external" {
		print("Настройки: внешняя флеш — подставьте шину драйвера в crates-cross/bsp/src/external.rs (поле Board::config); хранилище над ней (domain::settings) строит приложение и отдаёт узлу-потребителю.");
	} else {
		print("Настройки: раздел CONFIG во flash (crates-cross/bsp/src/flash.rs, поле Board::config); хранилище над ним (domain::settings) строит приложение и отдаёт узлу-потребителю.");
	}
}
if variable::get("graph") == "true" {
	print("Граф задач: узел HEARTBEAT в crates-cross/app/src/graph.rs, его работа — в crates-host/domain/src/heartbeat.rs.");
} else {
	print("Без графа задач: узлы спавнит tasks::spawn_all (crates-cross/app/src/tasks.rs, подмодуль на задачу) — без сторожа (причина — выше).");
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
			let node = if proto == "modbus" { "MODBUS_" } else if proto == "protobuf" { "PROTOBUF_" } else if proto == "canopen" { "CANOPEN_" } else { "" };
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
		print("Modbus: карта регистров — crates-host/domain/src/modbus/map.rs (заготовка EmptyRegisters); одна на все узлы, тип подставляет crates-cross/app/src/modbus.rs (псевдоним Map, строится Map::default()).");
	}
	if variable::get("has_canopen") == "true" {
		print("CANopen: словарь объектов — crates-host/canopen/device.toml.");
	}
	if variable::get("has_protobuf") == "true" {
		print("protobuf: схема — crates-host/proto, обработчик — crates-host/domain/src/protobuf/service.rs.");
	}
}

// Lock-файлов шаблон не раздаёт: состав зависимостей зависит от ответов выше —
// при `ota=no` уходят `embassy-boot*`, при `config=yes` добавляется
// `sequential-storage`, при OTA — `salty` (`embedded-storage-async` — при
// любом из них), протокол приносит свои крейты. Один шаблонный lock
// подходил бы ровно одному сочетанию, а остальным ломал бы
// `cargo fetch --locked` в CI. Поэтому их
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
	print("Что заполнить под свою плату: crates-cross/bsp/src/resources.rs (распиновка),");
	print("crates-cross/app/src/graph.rs (граф задач), crates-host/domain/ (логика).");
} else {
	// Файла графа в таком проекте нет вовсе — его удаляет chip-select.rhai
	// вместе с отключением графа, и звать сюда некуда.
	print("Что заполнить под свою плату: crates-cross/bsp/src/resources.rs (распиновка),");
	print("crates-host/domain/ (логика).");
}
// Справочник упомянут ровно здесь, а не в списке «Дальше» выше: он нужен не
// на старте, а в тот момент, когда садишься за распиновку и вспоминаешь,
// что распиновку неоткуда взять, кроме даташита.
print("");
print("  cargo xtask pins SPI1   # какие выводы умеют этот блок на " + chip + ", DMA, тактирование");
