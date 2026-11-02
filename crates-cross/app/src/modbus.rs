//! Карта регистров Modbus — одна на все узлы `MODBUS_*` (docs/modules/app.md,
//! «Modbus: одна карта на все узлы `MODBUS_*`»). Свою карту
//! проект подставляет здесь — псевдонимом [`Map`]; узлы берут тип отсюда и
//! правок не требуют. Карта строится через `Map::default()` при первом
//! [`handle`], поэтому карте проекта нужен `Default` — или проект заменяет
//! эту одну строку своим конструктором. И `Send`: мьютекс лежит в обычном
//! `static` (`OnceLock`), а тот требует `Sync`, — карта с `Rc`, `&RefCell` или
//! ручками к `local`-слотам других узлов сюда не встанет.

use embassy_sync::once_lock::OnceLock;

/// Карта проекта — реализация `domain::modbus::Registers`. Сейчас пуста; проект
/// описывает свою в `domain` (заготовка — `crates-host/domain/src/modbus/map.rs`)
/// и ставит её сюда.
pub type Map = domain::modbus::EmptyRegisters;

/// Ручка карты, общей у всех узлов Modbus. Псевдоним: тип слота узла и
/// аргумента задачи должен быть конкретным.
pub type Handle = domain::modbus::SharedRegisters<Map>;

/// Сама карта под мьютексом — одна на запуск.
static MAP: OnceLock<domain::modbus::MapMutex<Map>> = OnceLock::new();

/// Ручка карты: первый вызов заводит карту, каждый следующий отдаёт ручку к
/// той же — по копии на узел.
pub fn handle() -> Handle {
    Handle::new(MAP.get_or_init(|| domain::modbus::MapMutex::new(Map::default())))
}
