//! Сеть платы поверх `embassy-net`: стек и его фоновая работа.
//!
//! Ничего из этого не знает чипа: устройство — любой
//! [`Driver`](embassy_net::driver::Driver), сокеты — стек `embassy-net`.
//! Плата собирает стек над своим устройством (Ethernet) и отдаёт его вместе с
//! фоновой работой (`Net`); транспорты на стеке строит compose-site.

use embassy_net::driver::Driver;

/// Сеть платы: фоновая работа стека для узла `NET` и сам стек, на котором
/// узлы открывают свои сокеты.
///
/// Пара, потому что их даёт один `embassy_net::new`; чипа она не знает —
/// устройство `D` подставляет плата.
pub struct Net<D: Driver + 'static> {
    /// Фоновая работа стека — уезжает в узел `NET`.
    pub runner: embassy_net::Runner<'static, D>,
    /// Стек — на нём узлы открывают свои сокеты.
    pub stack: embassy_net::Stack<'static>,
}

// Вручную: у `Runner` нет `Debug`.
impl<D: Driver + 'static> core::fmt::Debug for Net<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Net").finish_non_exhaustive()
    }
}
