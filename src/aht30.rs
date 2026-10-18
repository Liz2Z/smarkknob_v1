use embedded_hal::i2c::{I2c, SevenBitAddress};

const AHT30_ADDR: u8 = 0x38;
const CMD_INIT: [u8; 3] = [0xBE, 0x08, 0x00];
const CMD_TRIGGER: [u8; 3] = [0xAC, 0x33, 0x00];
const CMD_SOFT_RESET: [u8; 1] = [0xBA];

#[derive(Debug, Clone, Copy)]
pub struct Aht30Reading {
    pub temperature: f32,
    pub humidity: f32,
}

pub struct Aht30<I2C> {
    i2c: I2C,
}

impl<I2C> Aht30<I2C>
where
    I2C: I2c<SevenBitAddress>,
{
    pub fn new(i2c: I2C) -> Self {
        Self { i2c }
    }

    pub fn init(&mut self) -> Result<(), I2C::Error> {
        self.i2c.write(AHT30_ADDR, &CMD_SOFT_RESET)?;
        Self::delay_ms(20);

        self.i2c.write(AHT30_ADDR, &CMD_INIT)?;
        Self::delay_ms(10);

        esp_println::println!("AHT30 initialized");
        Ok(())
    }

    pub fn read(&mut self) -> Result<Aht30Reading, I2C::Error> {
        self.i2c.write(AHT30_ADDR, &CMD_TRIGGER)?;
        Self::delay_ms(80);

        let mut data = [0u8; 7];
        self.i2c.read(AHT30_ADDR, &mut data)?;

        let status = data[0];
        if (status & 0x80) != 0 {
            esp_println::println!("AHT30 busy, retrying...");
            Self::delay_ms(10);
            self.i2c.read(AHT30_ADDR, &mut data)?;
        }

        let humidity_raw =
            ((data[1] as u32) << 12) | ((data[2] as u32) << 4) | ((data[3] as u32) >> 4);
        let temperature_raw =
            (((data[3] & 0x0F) as u32) << 16) | ((data[4] as u32) << 8) | (data[5] as u32);

        let humidity = (humidity_raw as f32 / (1 << 20) as f32) * 100.0;
        let temperature = (temperature_raw as f32 / (1 << 20) as f32) * 200.0 - 50.0;

        Ok(Aht30Reading {
            temperature,
            humidity,
        })
    }

    fn delay_ms(ms: u32) {
        let delay_start = esp_hal::time::Instant::now();
        while delay_start.elapsed() < esp_hal::time::Duration::from_millis(ms as u64) {}
    }
}
