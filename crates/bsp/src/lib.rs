#![no_std]
#![no_main]

pub struct Board {
    p: embassy_stm32::Peripherals,
}

impl Board {
    pub fn init() -> Self {
        let p = embassy_stm32::init(embassy_stm32::Config::default());

        Self { p }
    }
}

#[cfg(test)]
#[embedded_test::tests]
mod tests {
    use super::*;

    #[init]
    fn init() -> Board {
        Board::init()
    }

    #[test]
    fn test_works(board: Board) {
        assert!(true);
    }
}
