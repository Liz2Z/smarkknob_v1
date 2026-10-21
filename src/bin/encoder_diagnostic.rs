#![no_std]
#![no_main]

// Independent AS5600 read-only audit: no Wi-Fi, RTOS, PWM, or OTP writes.
use core::fmt::Write;
use esp_hal::{
    clock::CpuClock,
    delay::Delay,
    gpio::{Level, Output, OutputConfig},
    i2c::master::{Config, Error, I2c},
    time::Rate,
    usb_serial_jtag::UsbSerialJtag,
};

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    let _enable = Output::new(p.GPIO7, Level::Low, OutputConfig::default());
    let _phases = [
        Output::new(p.GPIO4, Level::Low, OutputConfig::default()),
        Output::new(p.GPIO5, Level::Low, OutputConfig::default()),
        Output::new(p.GPIO6, Level::Low, OutputConfig::default()),
    ];
    let mut usb = UsbSerialJtag::new(p.USB_DEVICE);
    let mut bus = I2c::new(
        p.I2C0,
        Config::default().with_frequency(Rate::from_khz(100)),
    )
    .unwrap()
    .with_sda(p.GPIO20)
    .with_scl(p.GPIO21);
    for sample in 0..20 {
        let read = (|| -> Result<_, Error> {
            let mut status = [0];
            let mut raw = [0; 2];
            let mut agc = [0];
            let mut magnitude = [0; 2];
            let mut burst = [0; 3];
            bus.write_read(0x36u8, &[0x0b], &mut status)?;
            bus.write_read(0x36u8, &[0x0c], &mut raw)?;
            bus.write_read(0x36u8, &[0x1a], &mut agc)?;
            bus.write_read(0x36u8, &[0x1b], &mut magnitude)?;
            bus.write_read(0x36u8, &[0x0b], &mut burst)?;
            Ok((
                status[0],
                u16::from_be_bytes(raw) & 0xfff,
                agc[0],
                u16::from_be_bytes(magnitude) & 0xfff,
                burst,
            ))
        })();
        let _ = writeln!(usb, "AUDIT sample={sample} result={read:?} EN=0");
        Delay::new().delay_millis(250);
    }
    let _ = writeln!(usb, "AUDIT DONE motor remains disabled");
    idle()
}

fn idle() -> ! {
    loop {
        // SAFETY: only sleep; EN was initialized LOW and is never enabled.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    idle()
}
