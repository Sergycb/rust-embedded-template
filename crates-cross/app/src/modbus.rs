//! Карта регистров Modbus — одна на все узлы `MODBUS_*`, по узлу на
//! транспорт: запись через один транспорт видна чтению через другой.
//!
//! Карта лежит под мьютексом `domain::modbus::MapMutex` в `static` этого
//! модуля, а узлы получают копии ручки [`Handle`] (`crate::parts::Parts`
//! собирает её один раз). Свою карту проект подставляет здесь —
//! псевдонимом [`Map`]; узлы берут тип отсюда и правок не требуют. [`new`]
//! строит карту через `Map::default()`, поэтому карте проекта нужен `Default`
//! — или проект заменяет эту одну строку в [`new`] своим конструктором.

use static_cell::StaticCell;

/// Карта проекта — реализация `ports::Registers`. Сейчас пуста; проект
/// описывает свою в `domain` и ставит её сюда.
pub type Map = domain::modbus::EmptyRegisters;

/// Ручка карты, общая у всех узлов Modbus. Псевдоним: тип слота узла и
/// аргумента задачи должен быть конкретным.
pub type Handle = domain::modbus::SharedRegisters<Map>;

/// Сама карта под мьютексом. `StaticCell`, а не обычный `static`: мьютекс на
/// `NoopRawMutex` не `Sync`.
static MAP: StaticCell<domain::modbus::MapMutex<Map>> = StaticCell::new();

/// Заводит карту и отдаёт ручку к ней; копий ручки — по одной на узел.
///
/// # Panics
///
/// Если позвать второй раз: карта одна на запуск.
#[must_use]
pub fn new() -> Handle {
    Handle::new(MAP.init(domain::modbus::MapMutex::new(Map::default())))
}
