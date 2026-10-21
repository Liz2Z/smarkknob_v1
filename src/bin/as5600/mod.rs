use alloc::boxed::Box;
use core::{cell::RefCell, ffi::c_void, fmt::Write};
use esp_hal::{
    Blocking,
    i2c::master::{Error, I2c},
    time::{Duration, Instant},
};
use motor_control::encoder::{Reading, Snapshot, feedback_configuration};

// Keep feedback above the Wi-Fi SDK work: the lower priority allowed 5.7ms
// update gaps during hand movement. Network requests are now independent of
// motor lifetime, so delayed dashboard responses cannot terminate a session.
// Both feedback tasks still sleep to leave radio/background work CPU time.
pub const ACTIVE_PRIORITY: usize = 30;

#[derive(Clone, Copy)]
struct Data {
    sample: Snapshot,
    running: bool,
    sequence: u32,
    errors: u32,
    configuration: Option<u16>,
}

pub struct Encoder {
    data: critical_section::Mutex<RefCell<Data>>,
}

#[derive(Clone, Copy)]
pub struct Telemetry(Data);

pub static ENCODER: Encoder = Encoder {
    data: critical_section::Mutex::new(RefCell::new(Data {
        sample: Snapshot {
            reading: None,
            sampled_at: None,
            error: None,
        },
        running: false,
        sequence: 0,
        errors: 0,
        configuration: None,
    })),
};

struct Worker {
    bus: I2c<'static, Blocking>,
    state: &'static Encoder,
}

impl Encoder {
    /// The task owns the bus. Shared state is copied under a short critical
    /// section; no lock is held during I2C, delay, or formatting.
    pub fn start(&'static self, bus: I2c<'static, Blocking>) {
        let worker = Box::new(Worker { bus, state: self });
        fn assert_send<T: Send>(_: &T) {}
        assert_send(&*worker);
        // SAFETY: the Box is uniquely owned, Send, and transferred once to the
        // never-returning task. The shared Encoder is static and synchronized.
        let handle = unsafe {
            esp_radio_rtos_driver::task_create(
                "as5600",
                worker_entry,
                Box::into_raw(worker).cast(),
                0,
                Some(0),
                6144,
            )
        };
        assert!(!handle.is_null());
    }

    pub fn set_running(&self, running: bool) {
        critical_section::with(|cs| self.data.borrow(cs).borrow_mut().running = running);
    }

    pub fn snapshot(&self) -> Snapshot {
        self.data().sample
    }

    pub fn diagnostic(&self) -> (u32, u32, Option<u64>) {
        let data = self.data();
        (data.sequence, data.errors, data.sample.sampled_at)
    }

    fn data(&self) -> Data {
        critical_section::with(|cs| *self.data.borrow(cs).borrow())
    }

    fn record(&self, result: Result<Reading, &'static str>) {
        let now = Instant::now().duration_since_epoch().as_micros();
        critical_section::with(|cs| {
            let mut data = self.data.borrow(cs).borrow_mut();
            if result.is_ok() {
                data.sequence = data.sequence.wrapping_add(1);
            } else {
                data.errors = data.errors.saturating_add(1);
            }
            data.sample.record(now, result);
        });
    }

    pub fn telemetry(&self) -> Telemetry {
        Telemetry(self.data())
    }
}

impl Telemetry {
    pub fn append_json(&self, out: &mut heapless::String<1024>, now: u64) {
        let data = self.0;
        let state = data.sample;
        let _ = write!(
            out,
            ",\"encoder\":{{\"online\":{},\"paused\":false,\"sample_seq\":{},\"errors\":{},\"target_hz\":{}",
            state.reading.is_some(),
            data.sequence,
            data.errors,
            if data.running { 1000 } else { 10 }
        );
        if let Some(configuration) = data.configuration {
            let _ = write!(out, ",\"conf\":{}", configuration);
        } else {
            let _ = out.push_str(",\"conf\":null");
        }
        if let Some(reading) = state.reading {
            let age = now.saturating_sub(state.sampled_at.unwrap_or(now)) / 1000;
            let valid = state
                .valid_angle(now, if data.running { 25_000 } else { 250_000 })
                .is_some();
            let _ = write!(
                out,
                ",\"raw_angle\":{},\"angle_valid\":{},\"status\":{},\"magnet\":\"{}\",\"age_ms\":{}",
                reading.raw_angle,
                valid,
                reading.status,
                reading.magnet(),
                age
            );
            if valid {
                let _ = write!(
                    out,
                    ",\"angle_deg\":{}",
                    motor_control::decimal::Fixed::<2>(reading.degrees())
                );
            } else {
                let _ = out.push_str(",\"angle_deg\":null");
            }
        } else {
            let _ = out.push_str(",\"raw_angle\":null,\"angle_valid\":false,\"angle_deg\":null,\"status\":null,\"magnet\":\"unknown\",\"age_ms\":null");
        }
        let _ = write!(out, ",\"error\":\"{}\"}}", state.error.unwrap_or(""));
    }
}

extern "C" fn worker_entry(param: *mut c_void) {
    // SAFETY: start() transferred this unique allocation to this task once.
    let Worker { bus, state } = *unsafe { Box::from_raw(param.cast::<Worker>()) };
    // A short blocking transfer on this preemptible worker avoids repeated
    // 100us RTOS sleeps/polls. Main can still preempt on the 1ms time slice.
    let mut bus = bus;
    let thread = esp_rtos::CurrentThreadHandle::get();
    let mut realtime = false;
    let mut original = None;
    let mut profile = None;
    loop {
        let started = Instant::now();
        let was_running = state.data().running;
        if was_running != realtime {
            thread.set_priority(if was_running { ACTIVE_PRIORITY } else { 0 });
            realtime = was_running;
        }
        if profile != Some(was_running) {
            match configure(&mut bus, &mut original, was_running) {
                Ok(configuration) => {
                    critical_section::with(|cs| {
                        state.data.borrow(cs).borrow_mut().configuration = Some(configuration)
                    });
                    profile = Some(was_running);
                }
                Err(error) => {
                    critical_section::with(|cs| {
                        state.data.borrow(cs).borrow_mut().configuration = None
                    });
                    state.record(Err(error));
                    thread.delay(Duration::from_millis(1000));
                    continue;
                }
            }
        }
        let result = read(&mut bus).map_err(bus_error);
        let failed = result.is_err();
        state.record(result);
        if failed {
            profile = None;
            // HAL timeout recovery can take up to 50ms on a stuck bus. It runs
            // only on this preemptible worker; back off to limit fault power.
            thread.delay(Duration::from_millis(1000));
            continue;
        }
        let period = if was_running { 1_000 } else { 100_000 };
        let deadline = started + Duration::from_micros(period);
        loop {
            let current = Instant::now();
            if current >= deadline || state.data().running != was_running {
                break;
            }
            let remaining = deadline - current;
            thread.delay(remaining.min(Duration::from_millis(5)));
        }
    }
}

fn bus_error(error: Error) -> &'static str {
    match error {
        Error::AcknowledgeCheckFailed(_) => "nack",
        Error::Timeout => "timeout",
        _ => "i2c",
    }
}

fn configure(
    bus: &mut I2c<'_, Blocking>,
    original: &mut Option<u16>,
    active: bool,
) -> Result<u16, &'static str> {
    let mut bytes = [0; 2];
    bus.write_read(0x36u8, &[0x07], &mut bytes)
        .map_err(bus_error)?;
    let current = u16::from_be_bytes(bytes);
    let saved = *original.get_or_insert(current);
    let desired = if active {
        feedback_configuration(saved)
    } else {
        saved
    };
    if current != desired {
        let bytes = desired.to_be_bytes();
        bus.write(0x36u8, &[0x07, bytes[0], bytes[1]])
            .map_err(bus_error)?;
    }
    bus.write_read(0x36u8, &[0x07], &mut bytes)
        .map_err(bus_error)?;
    if u16::from_be_bytes(bytes) != desired {
        return Err("config_verify");
    }
    Ok(desired)
}

fn read(bus: &mut I2c<'_, Blocking>) -> Result<Reading, Error> {
    // STATUS, RAW_ANGLE high, RAW_ANGLE low are consecutive. Starting at
    // STATUS uses normal address increment (datasheet I2C modes/register map).
    let mut registers = [0; 3];
    bus.write_read(0x36u8, &[0x0b], &mut registers)?;
    Ok(Reading::decode(registers[0], [registers[1], registers[2]]))
}
