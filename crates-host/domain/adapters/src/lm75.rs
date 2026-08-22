//! Датчик температуры LM75 по I2C — образец адаптера поверх шины.
//!
//! Шаблон не «про LM75»: датчик взят потому, что его протокол — два байта из
//! одного регистра, и на нём видно ровно то, что нужно показать, без страницы
//! регистровой карты. Важна форма, а не модель: драйвер generic по
//! [`embedded_hal_async::i2c::I2c`], поэтому живёт в `adapters` и проверяется
//! на хосте моком, а `bsp` лишь подаёт ему настоящую шину и кладёт готовый
//! объект полем в `Board`. Приложение видит его через
//! [`ports::TemperatureSensor`] и про I2C не знает.

use ports::TemperatureSensor;

/// Регистр температуры. Он же выбран по умолчанию после подачи питания, но
/// адресуется явно: любая операция с термостатом сменила бы указатель, и
/// чтение молча вернуло бы уставку вместо измерения.
const TEMPERATURE_REGISTER: u8 = 0x00;

/// Цена младшего разряда: 0.5 °C, то есть 500 миллиградусов.
const MILLICELSIUS_PER_TICK: i32 = 500;

pub struct Lm75<I2C> {
    i2c: I2C,
    address: u8,
}

impl<I2C> Lm75<I2C> {
    /// Адрес при всех трёх адресных выводах на земле.
    pub const DEFAULT_ADDRESS: u8 = 0x48;

    pub const fn new(i2c: I2C, address: u8) -> Self {
        Self { i2c, address }
    }
}

impl<I2C: embedded_hal_async::i2c::I2c> TemperatureSensor for Lm75<I2C> {
    /// Ошибка шины, какой её объявил драйвер: у порта тип ассоциированный
    /// именно ради этого — набор отказов у каждой шины свой.
    type Error = I2C::Error;

    async fn read_millicelsius(&mut self) -> Result<i32, Self::Error> {
        let mut raw = [0u8; 2];
        self.i2c
            .write_read(self.address, &[TEMPERATURE_REGISTER], &mut raw)
            .await?;

        // Девять значащих бит прижаты к старшему краю (D15..D7). Сдвиг идёт по
        // `i16`, то есть арифметический: на беззнаковом типе -25 °C
        // превратились бы в +487 °C.
        let ticks = i16::from_be_bytes(raw) >> 7;
        Ok(i32::from(ticks) * MILLICELSIUS_PER_TICK)
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use embedded_hal_mock::eh1::i2c::{Mock, Transaction};
    use ports::TemperatureSensor;

    use super::Lm75;

    #[test]
    fn reads_positive_temperature() {
        // 25 °C — это 50 шагов по 0.5 °C, прижатые к старшему краю двух байт.
        let expectations = [Transaction::write_read(
            Lm75::<Mock>::DEFAULT_ADDRESS,
            vec![0x00],
            vec![0x19, 0x00],
        )];
        let mut i2c = Mock::new(&expectations);
        let mut sensor = Lm75::new(i2c.clone(), Lm75::<Mock>::DEFAULT_ADDRESS);

        assert_eq!(block_on(sensor.read_millicelsius()), Ok(25_000));

        i2c.done();
    }

    #[test]
    fn reads_negative_temperature() {
        // -25 °C в дополнительном коде. Ради этого случая тест и существует:
        // логический сдвиг вместо арифметического превратил бы его в +487 °C,
        // и на тёплом стенде это никогда бы не всплыло.
        let expectations = [Transaction::write_read(
            Lm75::<Mock>::DEFAULT_ADDRESS,
            vec![0x00],
            vec![0xE7, 0x00],
        )];
        let mut i2c = Mock::new(&expectations);
        let mut sensor = Lm75::new(i2c.clone(), Lm75::<Mock>::DEFAULT_ADDRESS);

        assert_eq!(block_on(sensor.read_millicelsius()), Ok(-25_000));

        i2c.done();
    }
}
