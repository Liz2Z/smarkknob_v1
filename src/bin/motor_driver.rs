#![no_std]
#![no_main]

#[cfg(feature = "motor-http")]
extern crate alloc;
#[cfg(feature = "motor-http")]
mod as5600;
#[cfg(feature = "motor-http")]
mod motor_http;

use core::fmt::Write as _;
use esp_hal::{
    Blocking,
    clock::CpuClock,
    gpio::{
        AnyPin, DriveMode, Flex, Input, InputConfig, Level, Output, OutputConfig, Pull,
        interconnect::InputSignal,
    },
    ledc::{
        LSGlobalClkSource, Ledc, LowSpeed,
        channel::{self, Channel, ChannelHW, ChannelIFace},
        timer::{self, TimerIFace},
    },
    time::{Instant, Rate},
    usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagTx},
};
use motor_control::Run;

esp_bootloader_esp_idf::esp_app_desc!();

struct Driver<'a> {
    phases: [Channel<'a, LowSpeed>; 3],
    feedback: [InputSignal<'a>; 3],
    enable: Output<'a>,
}

impl Driver<'_> {
    fn stop(&mut self) {
        // Disable the bridge before changing PWM, including on faults.
        self.enable.set_low();
        for phase in &self.phases {
            phase.set_duty_hw(0);
        }
    }

    fn apply(&mut self, duties: [u32; 3]) {
        for (phase, duty) in self.phases.iter().zip(duties) {
            phase.set_duty_hw(duty);
        }
        self.enable.set_high();
    }

    /// Count actual GPIO pad transitions while the bridge stays disabled.
    fn diagnose_pwm(&mut self, fault: &Input<'_>) -> ([u32; 3], u32) {
        self.stop();
        for phase in &self.phases {
            phase.set_duty_hw(motor_control::PWM_PERIOD / 2);
        }
        let mut previous = self.feedback.each_ref().map(|pin| pin.is_input_high());
        let mut edges = [0; 3];
        let mut fault_low_samples = 0;
        let started = now_us();
        while now_us() - started < 20_000 {
            fault_low_samples += u32::from(fault.is_low());
            for i in 0..3 {
                let high = self.feedback[i].is_input_high();
                if high != previous[i] {
                    edges[i] += 1;
                    previous[i] = high;
                }
            }
        }
        self.stop();
        (edges, fault_low_samples)
    }
}

/// USB output must never block the automatic motor-stop deadline.
fn reply(tx: &mut UsbSerialJtagTx<'_, Blocking>, message: &[u8]) {
    for &byte in message {
        if tx.write_byte_nb(byte).is_err() {
            break;
        }
    }
    let _ = tx.flush_tx_nb();
}

fn now_us() -> u64 {
    Instant::now().duration_since_epoch().as_micros()
}

#[cfg(feature = "motor-http")]
fn http_status(
    run: &Run,
    fault: bool,
    encoder: &as5600::Encoder,
    control_max_gap_us: u64,
) -> motor_http::Response {
    let parameters = run.parameters();
    let mut body = heapless::String::new();
    let mode = if run.is_continuous() {
        "continuous"
    } else if run.active() {
        "pulse"
    } else {
        "stopped"
    };
    let direction = if run.reverse_setting() {
        "reverse"
    } else {
        "forward"
    };
    let _ = write!(
        body,
        "{{\"running\":{},\"fault\":{},\"mode\":\"{}\",\"direction\":\"{}\",\"voltage\":{:.3},\"frequency_hz\":{:.3},\"lease_ms\":500,\"uptime_ms\":{},\"control_max_gap_us\":{}",
        run.active(),
        fault,
        mode,
        direction,
        parameters.voltage,
        parameters.frequency_hz,
        now_us() / 1000,
        control_max_gap_us
    );
    encoder.append_json(&mut body, now_us());
    let _ = body.push('}');
    motor_http::Response::json(200, body)
}

#[esp_hal::main]
fn main() -> ! {
    // Wi-Fi supports 80MHz. Keep APB/LEDC at their existing clock while reducing
    // CPU power; the 500us control deadline is independent of the CPU clock.
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::_80MHz));
    // EN must be low before PWM is configured. Nothing moves on boot.
    let enable = Output::new(peripherals.GPIO7, Level::Low, OutputConfig::default());
    // nFAULT is open-drain; pull it up to the C3's 3.3V logic supply. A high
    // reading is meaningful only after nFT has actually been wired to GPIO10.
    let mut fault = Input::new(
        peripherals.GPIO10,
        InputConfig::default().with_pull(Pull::Up),
    );
    let mut ledc = Ledc::new(peripherals.LEDC);
    ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
    let mut timer = ledc.timer::<LowSpeed>(timer::Number::Timer0);
    timer
        .configure(timer::config::Config {
            duty: timer::config::Duty::Duty11Bit,
            clock_source: timer::LSClockSource::APBClk,
            frequency: Rate::from_hz(20_000),
        })
        .unwrap();
    let pins = [
        Flex::new(peripherals.GPIO4),
        Flex::new(peripherals.GPIO5),
        Flex::new(peripherals.GPIO6),
    ];
    // Keep the input stage enabled for diagnostics without stealing GPIOs or
    // changing the PWM output routing after configuration.
    let [(input4, output4), (input5, output5), (input6, output6)] = pins.map(|mut pin| {
        pin.apply_output_config(&OutputConfig::default().with_drive_mode(DriveMode::PushPull));
        pin.set_output_enable(true);
        pin.split()
    });
    let mut phases = [
        ledc.channel(channel::Number::Channel0, output4),
        ledc.channel(channel::Number::Channel1, output5),
        ledc.channel(channel::Number::Channel2, output6),
    ];
    for phase in &mut phases {
        phase
            .configure(channel::config::Config {
                timer: &timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .unwrap();
    }
    let mut driver = Driver {
        phases,
        feedback: [input4, input5, input6],
        enable,
    };
    driver.stop();
    let (mut rx, mut tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    #[cfg(feature = "motor-http")]
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);
    #[cfg(feature = "motor-http")]
    // Leave headroom for the encoder task stack and Wi-Fi association/RX
    // allocations. With 32KiB ordinary heap only ~2KiB remained at boot.
    esp_alloc::heap_allocator!(size: 48 * 1024);
    #[cfg(feature = "motor-http")]
    let radio = {
        let timg = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        let software =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg.timer0, software.software_interrupt0);
        esp_radio::init().unwrap()
    };
    #[cfg(feature = "motor-http")]
    let mut server = motor_http::Server::new(&radio, peripherals.WIFI, now_us());
    #[cfg(feature = "motor-http")]
    let main_thread = esp_rtos::CurrentThreadHandle::get();
    #[cfg(feature = "motor-http")]
    main_thread.set_priority(1);
    #[cfg(feature = "motor-http")]
    let encoder = &as5600::ENCODER;
    #[cfg(feature = "motor-http")]
    {
        let bus = esp_hal::i2c::master::I2c::new(
            peripherals.I2C0,
            esp_hal::i2c::master::Config::default()
                .with_frequency(Rate::from_khz(100))
                .with_timeout(esp_hal::i2c::master::BusTimeout::BusCycles(1000))
                .with_software_timeout(esp_hal::i2c::master::SoftwareTimeout::Transaction(
                    esp_hal::time::Duration::from_millis(20),
                )),
        )
        .unwrap()
        .with_sda(peripherals.GPIO20)
        .with_scl(peripherals.GPIO21);
        encoder.start(bus);
    }

    #[cfg(feature = "motor-http")]
    let mut http_owned = false;
    #[cfg(feature = "motor-http")]
    let mut http_deadline: Option<u64> = None;
    let mut run = Run::default();
    let mut line = [0u8; 32];
    let mut length = 0;
    let mut overflow = false;
    let mut armed_at: Option<u64> = None;
    let mut last_tick = now_us();
    #[cfg(feature = "motor-http")]
    let mut control_max_gap_us = 0u64;
    #[cfg(feature = "motor-http")]
    let mut control_last_us: Option<u64> = None;
    reply(&mut tx, b"READY motor-driver GPIO4/5/6 EN7; stopped\r\n");
    #[cfg(feature = "motor-http")]
    reply(
        &mut tx,
        b"POWER CPU80 idle=5ms sleep; WiFi idle=min/active=none\r\n",
    );

    loop {
        // Bound receive work, even if a host continuously floods USB.
        for _ in 0..64 {
            let Ok(byte) = rx.read_byte() else { break };
            if byte == 3 || byte == 27 {
                run.stop();
                driver.stop();
                armed_at = None;
                length = 0;
                overflow = false;
                reply(&mut tx, b"STOPPED\r\n");
            } else if byte == b'\n' || byte == b'\r' {
                if length == 0 && !overflow {
                    continue;
                }
                let command = &line[..length];
                let now = now_us();
                #[cfg(feature = "motor-http")]
                if matches!(command, b"arm" | b"s" | b"stop" | b"diag" | b"faultwire") {
                    http_owned = false;
                    http_deadline = None;
                }
                if overflow {
                    reply(&mut tx, b"ERROR command too long; STOPPED\r\n");
                } else if command == b"arm" {
                    run.stop();
                    driver.stop();
                    armed_at = Some(now);
                    reply(&mut tx, b"ARMED 10s; c continuous; f/r pulse; s stop\r\n");
                } else if command == b"f" || command == b"r" || command == b"c" {
                    if fault.is_low() {
                        run.stop();
                        driver.stop();
                        armed_at = None;
                        reply(&mut tx, b"FAULT LOW STOPPED; GPIO10/nFT asserted\r\n");
                    } else if run.active() {
                        reply(&mut tx, b"BUSY test deadline unchanged\r\n");
                    } else if armed_at.take().is_some_and(|at| now - at <= 10_000_000) {
                        #[cfg(feature = "motor-http")]
                        server.set_motor_running(true);
                        #[cfg(feature = "motor-http")]
                        {
                            control_max_gap_us = 0;
                            control_last_us = None;
                        }
                        // The power-mode transition may take time. Start the
                        // control deadline only after it has completed.
                        let now = now_us();
                        if command == b"c" {
                            run.start_continuous(now);
                            let parameters = run.parameters();
                            let mut message = heapless::String::<96>::new();
                            let _ = write!(
                                message,
                                "RUN continuous {:.2}V {:.2}Hz; keepalive 500ms\r\n",
                                parameters.voltage, parameters.frequency_hz
                            );
                            reply(&mut tx, message.as_bytes());
                        } else {
                            run.start(now, command == b"r");
                            reply(&mut tx, b"RUN 1s 0.5Hz cmd=12V clipped; align 0.2s\r\n");
                        }
                    } else {
                        reply(&mut tx, b"REJECTED send arm first\r\n");
                    }
                } else if command == b"k" {
                    #[cfg(feature = "motor-http")]
                    if http_owned {
                        reply(&mut tx, b"REJECTED HTTP owns motor keepalive\r\n");
                        length = 0;
                        overflow = false;
                        continue;
                    }
                    if run.keep_alive(now) {
                        reply(&mut tx, b"ALIVE\r\n");
                    } else {
                        run.stop();
                        driver.stop();
                        armed_at = None;
                        reply(&mut tx, b"STOPPED no live continuous lease\r\n");
                    }
                } else if command == b"s" || command == b"stop" {
                    run.stop();
                    driver.stop();
                    armed_at = None;
                    reply(&mut tx, b"STOPPED\r\n");
                } else if command == b"faultwire" {
                    // Passive input bias test only. Do not assume a HIGH
                    // level means the open-drain nFAULT is actually wired.
                    run.stop();
                    driver.stop();
                    armed_at = None;
                    fault.apply_config(&InputConfig::default().with_pull(Pull::Up));
                    let started = now_us();
                    while now_us() - started < 2_000 {
                        core::hint::spin_loop();
                    }
                    let up = fault.is_high();
                    fault.apply_config(&InputConfig::default().with_pull(Pull::Down));
                    let started = now_us();
                    while now_us() - started < 2_000 {
                        core::hint::spin_loop();
                    }
                    let down = fault.is_high();
                    fault.apply_config(&InputConfig::default().with_pull(Pull::Up));
                    let mut message = heapless::String::<96>::new();
                    let _ = write!(
                        message,
                        "FAULTWIRE UP={} DOWN={} RESTORED=UP EN=0\r\n",
                        u8::from(up),
                        u8::from(down)
                    );
                    reply(&mut tx, message.as_bytes());
                } else if command == b"fault" {
                    reply(
                        &mut tx,
                        if fault.is_low() {
                            b"FAULT LOW; nFT asserted if wired to GPIO10\r\n"
                        } else {
                            b"FAULT HIGH; valid only with nFT wired to GPIO10\r\n"
                        },
                    );
                } else if command == b"diag" {
                    run.stop();
                    armed_at = None;
                    let (edges, fault_low_samples) = driver.diagnose_pwm(&fault);
                    let mut message = heapless::String::<96>::new();
                    let _ = write!(
                        message,
                        "DIAG 20ms edges={}/{}/{} nFTlow={} EN=0\r\n",
                        edges[0], edges[1], edges[2], fault_low_samples
                    );
                    reply(&mut tx, message.as_bytes());
                } else if command == b"?" || command == b"status" {
                    if run.is_continuous() {
                        let parameters = run.parameters();
                        let mut message = heapless::String::<96>::new();
                        let _ = write!(
                            message,
                            "STATUS continuous {:.2}V {:.2}Hz\r\n",
                            parameters.voltage, parameters.frequency_hz
                        );
                        reply(&mut tx, message.as_bytes());
                    } else {
                        reply(
                            &mut tx,
                            if run.active() {
                                b"STATUS running\r\n"
                            } else {
                                b"STATUS stopped GPIO4/5/6 EN7 bus=12V cmd=12V clipped 0.5Hz\r\n"
                            },
                        );
                    }
                } else if command == b"encoder" {
                    #[cfg(feature = "motor-http")]
                    {
                        let (seq, errors, at) = encoder.diagnostic();
                        let mut message = heapless::String::<128>::new();
                        let age = at.map(|at| now_us().saturating_sub(at) / 1000);
                        let _ = write!(
                            message,
                            "ENC seq={} errors={} age_ms={:?} heap={}\r\n",
                            seq,
                            errors,
                            age,
                            esp_alloc::HEAP.free()
                        );
                        reply(&mut tx, message.as_bytes());
                    }
                } else if command == b"net" {
                    #[cfg(feature = "motor-http")]
                    {
                        let mut message = heapless::String::<96>::new();
                        if let Some(ip) = server.address {
                            let _ = write!(message, "HTTP http://{}/\r\n", ip);
                        } else {
                            let _ = write!(
                                message,
                                "HTTP waiting LINK={} t={}ms\r\n",
                                server.link_connected(),
                                now_us() / 1000
                            );
                        }
                        reply(&mut tx, message.as_bytes());
                    }
                } else {
                    run.stop();
                    driver.stop();
                    armed_at = None;
                    reply(&mut tx, b"UNKNOWN; stopped. arm then c or f/r; s stop\r\n");
                }
                length = 0;
                overflow = false;
            } else if length < line.len() && !overflow {
                line[length] = byte;
                length += 1;
            } else {
                overflow = true;
                run.stop();
                driver.stop();
                armed_at = None;
            }
        }

        if run.active() && fault.is_low() {
            let elapsed = run.elapsed_us(now_us()).unwrap_or(0);
            run.stop();
            driver.stop();
            armed_at = None;
            let mut message = heapless::String::<96>::new();
            let _ = write!(message, "FAULT LOW STOPPED t={}us; GPIO10/nFT\r\n", elapsed);
            reply(&mut tx, message.as_bytes());
        }

        let now = now_us();
        if now - last_tick >= 500 {
            last_tick = now;
            let was_active = run.active();
            #[cfg(feature = "motor-http")]
            if was_active {
                if let Some(previous) = control_last_us {
                    control_max_gap_us = control_max_gap_us.max(now.saturating_sub(previous));
                }
                control_last_us = Some(now);
            } else {
                control_last_us = None;
            }
            if let Some(duties) = run.update(now) {
                driver.apply(duties);
            } else {
                driver.stop();
                if was_active {
                    reply(&mut tx, b"STOPPED timeout, keepalive or update gap\r\n");
                }
            }
        }

        #[cfg(feature = "motor-http")]
        {
            if !run.is_continuous() {
                http_owned = false;
                http_deadline = None;
            }
            if http_owned && http_deadline.is_some_and(|deadline| now >= deadline) {
                run.stop();
                driver.stop();
                http_owned = false;
                http_deadline = None;
            }
            let action = server.poll(now, !run.active());
            if server.changed {
                if http_owned {
                    run.stop();
                    driver.stop();
                    http_owned = false;
                    http_deadline = None;
                }
                let mut message = heapless::String::<96>::new();
                if let Some(ip) = server.address {
                    let _ = write!(message, "HTTP http://{}/\r\n", ip);
                } else {
                    let _ = message.push_str("HTTP WiFi disconnected; stopped\r\n");
                }
                reply(&mut tx, message.as_bytes());
            }
            if let Some((connection, action)) = action {
                use motor_control::api::Action;
                let now = now_us();
                let response = match action {
                    Action::Status => {
                        http_status(&run, fault.is_low(), encoder, control_max_gap_us)
                    }
                    Action::Configure(parameters, reverse) => {
                        if reverse.is_some_and(|value| run.configure_direction(value)) {
                            driver.stop();
                            armed_at = None;
                            http_owned = false;
                            http_deadline = None;
                        }
                        run.configure(parameters);
                        http_status(&run, fault.is_low(), encoder, control_max_gap_us)
                    }
                    Action::Start(duration) => {
                        if fault.is_low() {
                            motor_http::Response::error(409, "driver fault; output disabled")
                        } else if run.active() {
                            motor_http::Response::error(409, "motor already running")
                        } else {
                            armed_at = None;
                            server.set_motor_running(true);
                            control_max_gap_us = 0;
                            control_last_us = None;
                            let now = now_us();
                            run.start_continuous(now);
                            http_owned = true;
                            http_deadline = if duration == 0 {
                                None
                            } else {
                                Some(now + duration as u64 * 1000)
                            };
                            http_status(&run, false, encoder, control_max_gap_us)
                        }
                    }
                    Action::Stop => {
                        run.stop();
                        driver.stop();
                        armed_at = None;
                        http_owned = false;
                        http_deadline = None;
                        http_status(&run, fault.is_low(), encoder, control_max_gap_us)
                    }
                    Action::Keepalive => {
                        if http_owned && run.keep_alive(now) {
                            http_status(&run, fault.is_low(), encoder, control_max_gap_us)
                        } else {
                            if http_owned {
                                run.stop();
                                driver.stop();
                                http_owned = false;
                                http_deadline = None;
                            }
                            motor_http::Response::error(409, "no active HTTP motor session")
                        }
                    }
                    Action::Page => unreachable!(),
                };
                server.respond(connection, response);
            }

            // All stop paths have disabled EN before returning to the idle
            // modem profile. Blocking here lets the RTOS run its WFI idle task
            // instead of spinning USB/TCP polls as fast as the CPU permits.
            server.set_motor_running(run.active());
            encoder.set_running(run.active());
            let deadline_us = if run.active() {
                last_tick.saturating_add(500)
            } else {
                now_us().saturating_add(5_000)
            };
            main_thread
                .delay_until(Instant::EPOCH + esp_hal::time::Duration::from_micros(deadline_us));
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: execution never returns to main; reclaim only the fixed EN pin
    // to shut down the bridge. No interrupt handler owns or writes GPIO7.
    let pin = unsafe { AnyPin::steal(7) };
    let mut enable = Output::new(pin, Level::Low, OutputConfig::default());
    enable.set_low();
    loop {
        core::hint::spin_loop();
    }
}
