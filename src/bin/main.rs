#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

use esp_hal::clock::CpuClock;
use esp_hal::main;
use esp_hal::time::{Duration, Instant};
use esp_hal::timer::timg::TimerGroup;
use panic_halt as _;

esp_bootloader_esp_idf::esp_app_desc!();

include!(concat!(env!("OUT_DIR"), "/wifi_config.rs"));

#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_println::println!("Step 1: Peripherals initialized");

    // 使用 reclaimed 方式分配更多堆内存
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 50 * 1024);

    esp_println::println!("Step 2: Heap allocated (50KB reclaimed)");

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);

    // ESP32-C3 (RISC-V) 需要两个参数
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    esp_println::println!("Step 3: RTOS started");

    esp_println::println!("Step 4: Initializing WiFi...");

    let controller = match esp_radio::init() {
        Ok(c) => c,
        Err(e) => {
            esp_println::println!("WiFi init failed: {:?}", e);
            loop {}
        }
    };

    esp_println::println!("Step 5: WiFi controller init...");

    // 参考项目使用引用方式
    let (mut wifi_controller, interfaces) =
        match esp_radio::wifi::new(&controller, peripherals.WIFI, Default::default()) {
            Ok(result) => result,
            Err(e) => {
                esp_println::println!("WiFi controller init failed: {:?}", e);
                loop {}
            }
        };

    esp_println::println!("Step 6: Configuring WiFi...");

    use esp_radio::wifi::{ClientConfig, ModeConfig};

    // 参考项目的方式：不设置 auth_method
    let client_config = ModeConfig::Client(
        ClientConfig::default()
            .with_ssid(WIFI_SSID.into())
            .with_password(WIFI_PASSWORD.into()),
    );

    if let Err(e) = wifi_controller.set_config(&client_config) {
        esp_println::println!("Failed to set WiFi config: {:?}", e);
        loop {}
    }

    esp_println::println!("Step 7: Starting WiFi...");

    if let Err(e) = wifi_controller.start() {
        esp_println::println!("Failed to start WiFi: {:?}", e);
        loop {}
    }

    esp_println::println!("Step 8: Connecting to WiFi: {}", WIFI_SSID);

    if let Err(e) = wifi_controller.connect() {
        esp_println::println!("Failed to connect: {:?}", e);
        loop {}
    }

    // 参考项目的等待方式
    loop {
        match wifi_controller.is_connected() {
            Ok(true) => {
                esp_println::println!("WiFi connected successfully!");
                break;
            }
            Ok(false) => {
                // 参考项目：Ok(false) 时只打印一次，不重复打印
            }
            Err(_err) => {
                esp_println::println!("WiFi connection error!");
                loop {}
            }
        }
        esp_println::println!("Waiting for WiFi connection...");
        let delay_start = Instant::now();
        while delay_start.elapsed() < Duration::from_millis(500) {}
    }

    esp_println::println!("Step 9: WiFi connected!");

    // 获取 MAC 地址
    let mac_addr = interfaces.sta.mac_address();
    esp_println::println!(
        "MAC Address: {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac_addr[0],
        mac_addr[1],
        mac_addr[2],
        mac_addr[3],
        mac_addr[4],
        mac_addr[5]
    );

    loop {
        esp_println::println!("Main loop running...");

        let delay_start = Instant::now();
        while delay_start.elapsed() < Duration::from_millis(5000) {}
    }
}
