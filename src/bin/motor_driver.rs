#![no_std]
#![no_main]

#[cfg(feature = "motor-http")]
extern crate alloc;
#[cfg(feature = "motor-http")]
mod as5600;
#[cfg(feature = "motor-http")]
mod motor_http;
#[cfg(feature = "motor-http")]
mod motor_trace;

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
use motor_control::{Run, decimal::Fixed};

// A panic in any RTOS worker must prevent main from re-enabling the bridge.
static EMERGENCY_STOP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "motor-http")]
static STATUS_FORMAT_MAX_US: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(feature = "motor-http")]
static NETWORK_POLL_MAX_US: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
#[cfg(feature = "motor-http")]
static CONTROL_WORK_MAX_US: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[cfg(feature = "motor-http")]
fn record_max(counter: &core::sync::atomic::AtomicU32, elapsed: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    critical_section::with(|_| {
        counter.store(
            counter
                .load(Relaxed)
                .max(elapsed.min(u32::MAX as u64) as u32),
            Relaxed,
        );
    });
}

#[cfg(feature = "motor-http")]
fn reset_timing() {
    use core::sync::atomic::Ordering::Relaxed;
    STATUS_FORMAT_MAX_US.store(0, Relaxed);
    NETWORK_POLL_MAX_US.store(0, Relaxed);
    CONTROL_WORK_MAX_US.store(0, Relaxed);
}

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
        critical_section::with(|_| {
            if EMERGENCY_STOP.load(core::sync::atomic::Ordering::Acquire) {
                self.stop();
                return;
            }
            for (phase, duty) in self.phases.iter().zip(duties) {
                phase.set_duty_hw(duty);
            }
            self.enable.set_high();
        });
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
    haptic: &motor_control::haptic::Haptics,
    control_max_gap_us: u64,
) -> motor_http::Response {
    let mode = if haptic.active() {
        "haptic"
    } else if run.is_continuous() {
        "continuous"
    } else if run.active() {
        "pulse"
    } else {
        "stopped"
    };
    motor_http::Response::status(motor_http::Status {
        running: run.active() || haptic.active(),
        fault,
        mode,
        reverse: run.reverse_setting(),
        parameters: run.parameters(),
        captured_us: now_us(),
        control_max_gap_us,
        encoder: encoder.telemetry(),
        knob: motor_http::KnobStatus {
            mode: haptic.config.mode.name(),
            active: haptic.active(),
            calibrated: haptic.calibrated(),
            phase: haptic.phase(),
            position: haptic.position,
            angle_deg: haptic.angle_deg(),
            strength: haptic.config.strength,
            step_deg: haptic.config.step_deg,
            voltage: haptic.commanded_volts,
            error: haptic.error,
            sensor_direction: haptic.sensor_direction(),
        },
    })
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
    // Include the dedicated HTTP worker stack and preserve Wi-Fi RX/TX
    // headroom. At 48KiB only 2.8KiB remained after worker initialization,
    // causing packet drops/timeouts even while control timing was healthy.
    esp_alloc::heap_allocator!(size: 64 * 1024);
    #[cfg(feature = "motor-http")]
    let radio = {
        let timg = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
        let software =
            esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
        esp_rtos::start(timg.timer0, software.software_interrupt0);
        esp_radio::init().unwrap()
    };
    #[cfg(feature = "motor-http")]
    let mut server = motor_http::Server::new(
        alloc::boxed::Box::leak(alloc::boxed::Box::new(radio)),
        peripherals.WIFI,
        now_us(),
    );
    #[cfg(feature = "motor-http")]
    let main_thread = esp_rtos::CurrentThreadHandle::get();
    #[cfg(feature = "motor-http")]
    main_thread.set_priority(1);
    #[cfg(feature = "motor-http")]
    let mut realtime = false;
    #[cfg(feature = "motor-http")]
    let encoder = &as5600::ENCODER;
    #[cfg(feature = "motor-http")]
    {
        let bus = esp_hal::i2c::master::I2c::new(
            peripherals.I2C0,
            esp_hal::i2c::master::Config::default()
                .with_frequency(Rate::from_khz(400))
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
    #[cfg(feature = "motor-http")]
    let mut haptic = motor_control::haptic::Haptics::default();
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
                #[cfg(feature = "motor-http")]
                haptic.stop("");
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
                if !matches!(command, b"?" | b"status" | b"encoder" | b"net") && haptic.active() {
                    haptic.stop("serial_override");
                    driver.stop();
                    http_owned = false;
                    http_deadline = None;
                }
                #[cfg(feature = "motor-http")]
                if matches!(command, b"arm" | b"s" | b"stop" | b"diag" | b"faultwire") {
                    http_owned = false;
                    http_deadline = None;
                }
                if overflow {
                    reply(&mut tx, b"ERROR command too long; STOPPED\r\n");
                } else if command == b"arm" {
                    run.stop();
                    #[cfg(feature = "motor-http")]
                    haptic.stop("");
                    driver.stop();
                    armed_at = Some(now);
                    reply(&mut tx, b"ARMED 10s; c continuous; f/r pulse; s stop\r\n");
                } else if command == b"f" || command == b"r" || command == b"c" {
                    if fault.is_low() {
                        run.stop();
                        #[cfg(feature = "motor-http")]
                        haptic.stop("");
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
                            reset_timing();
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
                                "RUN continuous {}V {}Hz; keepalive 500ms\r\n",
                                Fixed::<2>(parameters.voltage),
                                Fixed::<2>(parameters.frequency_hz)
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
                        #[cfg(feature = "motor-http")]
                        haptic.stop("");
                        driver.stop();
                        armed_at = None;
                        reply(&mut tx, b"STOPPED no live continuous lease\r\n");
                    }
                } else if command == b"s" || command == b"stop" {
                    run.stop();
                    #[cfg(feature = "motor-http")]
                    haptic.stop("");
                    driver.stop();
                    armed_at = None;
                    reply(&mut tx, b"STOPPED\r\n");
                } else if command == b"faultwire" {
                    // Passive input bias test only. Do not assume a HIGH
                    // level means the open-drain nFAULT is actually wired.
                    run.stop();
                    #[cfg(feature = "motor-http")]
                    haptic.stop("");
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
                    #[cfg(feature = "motor-http")]
                    haptic.stop("");
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
                            "STATUS continuous {}V {}Hz\r\n",
                            Fixed::<2>(parameters.voltage),
                            Fixed::<2>(parameters.frequency_hz)
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
                    #[cfg(feature = "motor-http")]
                    haptic.stop("");
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
                #[cfg(feature = "motor-http")]
                haptic.stop("");
                driver.stop();
                armed_at = None;
            }
        }

        if (run.active() || {
            #[cfg(feature = "motor-http")]
            {
                haptic.active()
            }
            #[cfg(not(feature = "motor-http"))]
            {
                false
            }
        }) && (fault.is_low() || EMERGENCY_STOP.load(core::sync::atomic::Ordering::Acquire))
        {
            let elapsed = run.elapsed_us(now_us()).unwrap_or(0);
            run.stop();
            #[cfg(feature = "motor-http")]
            haptic.stop(
                if EMERGENCY_STOP.load(core::sync::atomic::Ordering::Acquire) {
                    "internal_error"
                } else {
                    ""
                },
            );
            driver.stop();
            armed_at = None;
            if EMERGENCY_STOP.load(core::sync::atomic::Ordering::Acquire) {
                reply(&mut tx, b"INTERNAL ERROR STOPPED; restart required\r\n");
            } else {
                let mut message = heapless::String::<96>::new();
                let _ = write!(message, "FAULT LOW STOPPED t={}us; GPIO10/nFT\r\n", elapsed);
                reply(&mut tx, message.as_bytes());
            }
        }

        let now = now_us();
        // AS5600 acquisition and force control both target 1kHz. Sleep leaves
        // CPU time for I2C while retaining the legacy 2kHz open-loop schedule.
        let control_period_us = {
            #[cfg(feature = "motor-http")]
            {
                if haptic.active() { 1000 } else { 500 }
            }
            #[cfg(not(feature = "motor-http"))]
            {
                500
            }
        };
        if now - last_tick >= control_period_us {
            last_tick = now;
            let was_active = run.active() || {
                #[cfg(feature = "motor-http")]
                {
                    haptic.active()
                }
                #[cfg(not(feature = "motor-http"))]
                {
                    false
                }
            };
            #[cfg(feature = "motor-http")]
            if was_active {
                if let Some(previous) = control_last_us {
                    control_max_gap_us = control_max_gap_us.max(now.saturating_sub(previous));
                }
                control_last_us = Some(now);
            } else {
                control_last_us = None;
            }
            #[cfg(feature = "motor-http")]
            let control_started = now_us();
            #[cfg(feature = "motor-http")]
            let mut feedback_sample = None;
            #[cfg(feature = "motor-http")]
            let output = if haptic.active() {
                let sample = encoder.snapshot();
                feedback_sample = Some(sample);
                haptic.update(now_us(), sample)
            } else {
                run.update(now)
            };
            #[cfg(not(feature = "motor-http"))]
            let output = run.update(now);
            if let Some(duties) = output {
                driver.apply(duties);
            } else {
                driver.stop();
                if was_active && !run.active() && {
                    #[cfg(feature = "motor-http")]
                    {
                        !haptic.active()
                    }
                    #[cfg(not(feature = "motor-http"))]
                    {
                        true
                    }
                } {
                    reply(&mut tx, b"STOPPED duration, control gap or feedback\r\n");
                }
            }
            // Apply PWM before trace conversions/copying. Its timestamp now
            // describes the actual actuation boundary, rather than an earlier
            // command that still has diagnostics work ahead of it.
            #[cfg(feature = "motor-http")]
            if let Some(sample) = feedback_sample {
                let applied_at = now_us();
                motor_trace::record(
                    applied_at,
                    sample,
                    &haptic,
                    applied_at.saturating_sub(control_started),
                );
            }
            #[cfg(feature = "motor-http")]
            if was_active {
                record_max(
                    &CONTROL_WORK_MAX_US,
                    now_us().saturating_sub(control_started),
                );
            }
        }

        #[cfg(feature = "motor-http")]
        {
            if !run.is_continuous() && !haptic.active() {
                http_owned = false;
                http_deadline = None;
            }
            if http_owned && http_deadline.is_some_and(|deadline| now >= deadline) {
                run.stop();
                #[cfg(feature = "motor-http")]
                haptic.stop("");
                driver.stop();
                http_owned = false;
                http_deadline = None;
            }
            let poll_started = now_us();
            let action = server.poll(now, true);
            record_max(&NETWORK_POLL_MAX_US, now_us().saturating_sub(poll_started));
            if server.changed {
                let mut message = heapless::String::<96>::new();
                if let Some(ip) = server.address {
                    let _ = write!(message, "HTTP http://{}/\r\n", ip);
                } else {
                    let _ =
                        message.push_str("HTTP WiFi disconnected; board operation continues\r\n");
                }
                reply(&mut tx, message.as_bytes());
            }
            if let Some((connection, action)) = action {
                use motor_control::api::Action;
                let response = match action {
                    Action::Status => {
                        http_status(&run, fault.is_low(), encoder, &haptic, control_max_gap_us)
                    }
                    Action::Configure(parameters, reverse) => {
                        if haptic.active() {
                            haptic.stop("");
                            driver.stop();
                            http_owned = false;
                            http_deadline = None;
                        }

                        if reverse.is_some_and(|value| run.configure_direction(value)) {
                            driver.stop();
                            armed_at = None;
                            http_owned = false;
                            http_deadline = None;
                        }
                        run.configure(parameters);
                        http_status(&run, fault.is_low(), encoder, &haptic, control_max_gap_us)
                    }
                    Action::Start(duration) => {
                        if EMERGENCY_STOP.load(core::sync::atomic::Ordering::Acquire) {
                            motor_http::Response::error(409, "internal error; restart board")
                        } else if fault.is_low() {
                            motor_http::Response::error(409, "driver fault; output disabled")
                        } else if run.active() || haptic.active() {
                            motor_http::Response::error(409, "motor already running")
                        } else {
                            armed_at = None;
                            server.set_motor_running(true);
                            control_max_gap_us = 0;
                            reset_timing();
                            control_last_us = None;
                            let now = now_us();
                            run.start_autonomous(now);
                            http_owned = true;
                            http_deadline = if duration == 0 {
                                None
                            } else {
                                Some(now + duration as u64 * 1000)
                            };
                            http_status(&run, false, encoder, &haptic, control_max_gap_us)
                        }
                    }
                    Action::Stop => {
                        run.stop();
                        #[cfg(feature = "motor-http")]
                        haptic.stop("");
                        driver.stop();
                        armed_at = None;
                        http_owned = false;
                        http_deadline = None;
                        http_status(&run, fault.is_low(), encoder, &haptic, control_max_gap_us)
                    }
                    Action::Keepalive => {
                        // Compatibility endpoint for older pages. Operation is
                        // owned by the board and never depends on this request.
                        http_status(&run, fault.is_low(), encoder, &haptic, control_max_gap_us)
                    }
                    Action::HapticConfigure(config) => {
                        run.stop();
                        haptic.configure(config);
                        driver.stop();
                        armed_at = None;
                        http_owned = false;
                        http_deadline = None;
                        http_status(&run, fault.is_low(), encoder, &haptic, control_max_gap_us)
                    }
                    Action::HapticCalibrate | Action::HapticStart(_) => {
                        if EMERGENCY_STOP.load(core::sync::atomic::Ordering::Acquire) {
                            motor_http::Response::error(409, "internal error; restart board")
                        } else if fault.is_low() {
                            motor_http::Response::error(409, "driver fault; output disabled")
                        } else if run.active() || haptic.active() {
                            motor_http::Response::error(409, "stop before switching modes")
                        } else {
                            let calibrate = matches!(action, Action::HapticCalibrate);
                            server.set_motor_running(true);
                            let now = now_us();
                            let accepted = if calibrate {
                                haptic.calibrate(now)
                            } else {
                                haptic.start(now)
                            };
                            if accepted {
                                motor_trace::reset();
                                armed_at = None;
                                http_owned = true;
                                control_max_gap_us = 0;
                                reset_timing();
                                control_last_us = None;
                                http_deadline = match action {
                                    Action::HapticStart(0) => None,
                                    Action::HapticStart(ms) => Some(now + ms as u64 * 1000),
                                    _ => Some(now + 6_000_000),
                                };
                                encoder.set_running(true);
                                http_status(&run, false, encoder, &haptic, control_max_gap_us)
                            } else {
                                motor_http::Response::error(409, "calibrate before enabling force")
                            }
                        }
                    }
                    Action::Page | Action::HapticTrace => unreachable!(),
                };
                server.respond(connection, response);
            }

            // All stop paths have disabled EN before returning to the idle
            // modem profile. Blocking here lets the RTOS run its WFI idle task
            // instead of spinning USB/TCP polls as fast as the CPU permits.
            let active = run.active() || haptic.active();
            server.set_motor_running(active);
            encoder.set_running(active);
            let force_control = haptic.active();
            if realtime != force_control {
                main_thread.set_priority(if force_control {
                    as5600::ACTIVE_PRIORITY
                } else {
                    1
                });
                realtime = force_control;
            }
            let deadline_us = if active {
                let nominal = last_tick.saturating_add(control_period_us);
                if force_control {
                    // Heavy HTTP formatting can consume the entire nominal
                    // period. Reserve a real sleep window for Wi-Fi instead of
                    // repeatedly requesting an already-expired deadline at a
                    // higher priority and starving dashboard transport.
                    nominal.max(now_us().saturating_add(500))
                } else {
                    nominal
                }
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
    EMERGENCY_STOP.store(true, core::sync::atomic::Ordering::Release);
    // SAFETY: terminal emergency path reclaims only EN; the latch prevents
    // another RTOS task from subsequently turning the bridge back on.
    let pin = unsafe { AnyPin::steal(7) };
    let mut enable = Output::new(pin, Level::Low, OutputConfig::default());
    enable.set_low();
    loop {
        core::hint::spin_loop();
    }
}
