#![no_std]
#![no_main]

use esp_hal::{
    clock::CpuClock,
    gpio::{Level, Output, OutputConfig},
};

esp_bootloader_esp_idf::esp_app_desc!();

/// USB-only thermal comparison. No radio, networking, PWM, or sensor polling.
#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    let _enable = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    let _phase1 = Output::new(peripherals.GPIO4, Level::Low, OutputConfig::default());
    let _phase2 = Output::new(peripherals.GPIO5, Level::Low, OutputConfig::default());
    let _phase3 = Output::new(peripherals.GPIO6, Level::Low, OutputConfig::default());
    esp_println::println!("IDLE DIAGNOSTIC: radio off, motor disabled, CPU 80MHz, WFI");
    idle()
}

fn idle() -> ! {
    loop {
        // SAFETY: WFI waits for an interrupt; it does not access memory or IO.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    idle()
}
