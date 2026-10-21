//! Voltage-mode haptics inspired by SmartKnob's detent centers and snap hysteresis.
//! Independent implementation for this 7-pole-pair motor. No current regulation.
use crate::{MAX_UPDATE_GAP_US, angle_duties, encoder::Snapshot};
use core::f32::consts::{FRAC_PI_2, PI, TAU};

pub const POLE_PAIRS: f32 = 7.0;
const SENSOR_MAX_AGE: u64 = 25_000;
const CAL_VOLTS: f32 = 0.6;
const ALIGN_US: u64 = 1_000_000;
const SWEEP_US: u64 = 3_000_000;
const SETTLE_US: u64 = 500_000;
const ENGAGE_US: u64 = 150_000;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mode {
    Free,
    #[default]
    Detents,
    Switch,
    Selector,
    Spring,
    RatchetForward,
    RatchetReverse,
}
impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "free" => Self::Free,
            "detents" => Self::Detents,
            "switch" => Self::Switch,
            "selector" => Self::Selector,
            "spring" => Self::Spring,
            "ratchet_forward" => Self::RatchetForward,
            "ratchet_reverse" => Self::RatchetReverse,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Detents => "detents",
            Self::Switch => "switch",
            Self::Selector => "selector",
            Self::Spring => "spring",
            Self::RatchetForward => "ratchet_forward",
            Self::RatchetReverse => "ratchet_reverse",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub mode: Mode,
    pub strength: f32,
    pub step_deg: f32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Detents,
            strength: 0.4,
            step_deg: 15.0,
        }
    }
}
impl Config {
    pub fn valid(self) -> bool {
        self.strength.is_finite()
            && (0.1..=1.0).contains(&self.strength)
            && self.step_deg.is_finite()
            && (5.0..=90.0).contains(&self.step_deg)
    }
}
#[derive(Clone, Copy, Default)]
struct Calibration {
    valid: bool,
    direction: f32,
    zero: f32,
}
#[derive(Clone, Copy, PartialEq, Default)]
enum Phase {
    #[default]
    Stopped,
    WarmCalibration,
    Calibrating,
    WarmRun,
    Running,
}

#[derive(Default)]
pub struct Haptics {
    pub config: Config,
    calibration: Calibration,
    phase: Phase,
    started: u64,
    last_update: u64,
    last_sample: Option<u64>,
    raw: f32,
    unwrapped: f32,
    velocity: f32,
    velocity_angle: f32,
    force_angle: f32,
    fast_feedback_until: u64,
    coast_until: u64,
    engaged_at: u64,
    pub coasting: bool,
    origin: f32,
    center: f32,
    pub position: i32,
    cal_start: Option<f32>,
    cal_previous: f32,
    pub commanded_volts: f32,
    pub error: &'static str,
}
impl Haptics {
    pub fn active(&self) -> bool {
        self.phase != Phase::Stopped
    }
    pub fn calibrated(&self) -> bool {
        self.calibration.valid
    }
    pub fn phase(&self) -> &'static str {
        if self.phase == Phase::Running && self.coasting {
            return "coasting";
        }
        match self.phase {
            Phase::Stopped => "stopped",
            Phase::WarmCalibration | Phase::WarmRun => "warming",
            Phase::Calibrating => "calibrating",
            Phase::Running => "running",
        }
    }
    pub fn angle_deg(&self) -> f32 {
        (self.unwrapped - self.origin) * 180.0 / PI
    }
    pub fn velocity_rad_s(&self) -> f32 {
        self.velocity
    }
    pub fn detent_error_deg(&self) -> f32 {
        (self.unwrapped - self.origin - self.center) * 180.0 / PI
    }
    pub fn sensor_direction(&self) -> i32 {
        self.calibration.direction as i32
    }
    pub fn configure(&mut self, c: Config) -> bool {
        if !c.valid() {
            return false;
        }
        self.stop("");
        self.config = c;
        true
    }
    pub fn stop(&mut self, error: &'static str) {
        self.phase = Phase::Stopped;
        self.commanded_volts = 0.0;
        self.coasting = false;
        self.error = error;
    }
    fn begin(&mut self, now: u64, calibrate: bool) -> bool {
        if self.active() || (!calibrate && !self.calibrated() && self.config.mode != Mode::Free) {
            return false;
        }
        self.phase = if calibrate {
            Phase::WarmCalibration
        } else {
            Phase::WarmRun
        };
        self.started = now;
        self.last_update = now;
        self.last_sample = None;
        self.velocity = 0.0;
        self.fast_feedback_until = 0;
        self.coast_until = 0;
        self.coasting = false;
        self.position = 0;
        self.error = "";
        self.cal_start = None;
        if calibrate {
            self.calibration.valid = false;
        }
        true
    }
    pub fn start(&mut self, now: u64) -> bool {
        self.begin(now, false)
    }
    pub fn calibrate(&mut self, now: u64) -> bool {
        self.begin(now, true)
    }
    fn sample(&mut self, now: u64, s: Snapshot) -> bool {
        let Some(r) = s.reading else {
            self.error = "encoder_io";
            return false;
        };
        let Some(at) = s.sampled_at else {
            self.error = "encoder_time";
            return false;
        };
        if now < at {
            self.error = "encoder_clock";
            return false;
        }
        if now - at >= SENSOR_MAX_AGE {
            self.error = "encoder_stale";
            return false;
        }
        if r.status & 0x20 == 0 {
            self.error = "magnet_missing";
            return false;
        }
        if r.status & 0x18 == 0x18 {
            self.error = "magnet_invalid";
            return false;
        }
        self.error = "";
        if self.last_sample == Some(at) {
            return true;
        }
        let raw = r.degrees() * PI / 180.0;
        if let Some(last) = self.last_sample {
            if at <= last {
                self.error = "encoder_order";
                return false;
            }
            let delta = wrap(raw - self.raw);
            let dt = (at - last) as f32 * 1e-6;
            let speed = delta / dt;
            // SmartKnob also removes torque during very fast turns rather
            // than ending the mode. A single spike holds torque off briefly;
            // recovery requires filtered motion to be slow again.
            if speed.abs() > 60.0 {
                if self.phase == Phase::Calibrating {
                    self.stop("calibration_motion");
                    return false;
                }
                self.coasting = true;
                self.coast_until = now.saturating_add(150_000);
            }
            self.unwrapped += delta;
            let tracked_speed = if self.config.mode == Mode::Detents {
                // A 12-bit angle can alternate by one count without motion.
                // Direct differentiation at 1ms creates ~1rad/s false velocity.
                // Track within measurement uncertainty before differentiating;
                // sustained motion follows after this tiny angular band,
                // without a multi-millisecond low-pass time constant.
                let uncertainty = 1.5 * TAU / 4096.0;
                let residual = self.unwrapped - self.velocity_angle;
                let movement = residual - residual.clamp(-uncertainty, uncertainty);
                self.velocity_angle += movement;
                movement / dt
            } else {
                self.velocity_angle = self.unwrapped;
                speed
            };
            // The measured 43Hz ringing exposed ~3ms of velocity phase lag.
            // Detents need a faster derivative; their center dead band still
            // suppresses low-speed quantization noise.
            let filter = if self.config.mode == Mode::Detents {
                // Keep fast damping through a reversal/settling cycle. Simply
                // switching to a slow filter at zero velocity delays precisely
                // the next sign change and can restart release oscillation.
                if tracked_speed.abs().max(self.velocity.abs()) > 3.0 {
                    self.fast_feedback_until = now.saturating_add(20_000);
                }
                if now < self.fast_feedback_until {
                    0.0003
                } else {
                    0.002
                }
            } else {
                0.0015
            };
            self.velocity += (tracked_speed - self.velocity) * (dt / (filter + dt));
            // Position force also used the raw 12-bit staircase directly even
            // after derivative noise was removed. Smooth only the slow detent
            // position path; fast/release dynamics bypass this extra lag.
            // Counts, reported angle, commutation and calibration remain raw.
            if self.config.mode == Mode::Detents && now >= self.fast_feedback_until {
                self.force_angle += (self.unwrapped - self.force_angle) * (dt / (0.0005 + dt));
            } else {
                self.force_angle = self.unwrapped;
            }
        } else {
            self.unwrapped = raw;
            self.velocity_angle = raw;
            self.force_angle = raw;
        }
        self.raw = raw;
        self.last_sample = Some(at);
        true
    }
    /// None means bridge disabled, including free mode, completed calibration,
    /// and every fault. Network requests never sustain or restart a session.
    pub fn update(&mut self, now: u64, s: Snapshot) -> Option<[u32; 3]> {
        if !self.active() {
            return None;
        }
        if now < self.last_update || now - self.last_update > MAX_UPDATE_GAP_US {
            self.stop("control_gap");
            return None;
        }
        self.last_update = now;
        let fresh = self.sample(now, s);
        if !self.active() {
            return None;
        }
        if matches!(self.phase, Phase::WarmCalibration | Phase::WarmRun) {
            if !fresh {
                if now - self.started > 200_000 {
                    self.stop(self.error);
                }
                return None;
            }
            self.origin = self.unwrapped;
            self.center = 0.0;
            self.position = 0;
            self.started = now;
            self.engaged_at = now;
            self.phase = if self.phase == Phase::WarmCalibration {
                Phase::Calibrating
            } else {
                Phase::Running
            };
        }
        if !fresh {
            self.stop(self.error);
            return None;
        }
        if self.phase == Phase::Calibrating {
            return self.calibration_tick(now);
        }
        if self.coasting && now >= self.coast_until && self.velocity.abs() < 20.0 {
            self.coasting = false;
            self.engaged_at = now;
        }
        if self.config.mode == Mode::Free {
            self.commanded_volts = 0.0;
            return None;
        }
        let x = self.unwrapped - self.origin;
        // Predict from the timestamp of the actual sample, not the last loop.
        // 0.5ms includes the sensor/transfer and calculation-to-PWM latency.
        // Limit only the prediction horizon/extrapolation, never peak voltage.
        let prediction = if self.config.mode == Mode::Detents {
            let horizon =
                (now.saturating_sub(self.last_sample.unwrap_or(now)) + 500).min(2500) as f32 * 1e-6;
            let limit = self.config.step_deg * PI / 180.0 * 0.2;
            (self.velocity * horizon).clamp(-limit, limit)
        } else {
            0.0
        };
        let force_prediction = if self.config.mode == Mode::Detents {
            prediction + self.force_angle - self.unwrapped
        } else {
            prediction
        };
        let desired = force_with_prediction(
            self.config,
            x,
            self.velocity,
            &mut self.center,
            &mut self.position,
            force_prediction,
        );
        if self.coasting {
            self.commanded_volts = 0.0;
            return None;
        }
        // Ramp the envelope only when engaging, never the signed feedback.
        // Slewing +1V to -1V at 4V/s left torque pointing the wrong way for
        // 250ms after crossing a detent. Feedback must follow the current angle.
        let gain = ((now - self.engaged_at) as f32 / ENGAGE_US as f32).min(1.0);
        self.commanded_volts = desired * gain;
        let align = self.calibration.direction * POLE_PAIRS * (self.raw + prediction)
            + self.calibration.zero;
        // Keep all phases at the same duty at a well's center. Zero commanded
        // torque then retains passive winding/back-EMF damping instead of
        // alternately floating and re-enabling the rotor around the dead band.
        // Free/coasting/fault paths above still explicitly disable the bridge.
        Some(angle_duties(
            align + FRAC_PI_2,
            self.commanded_volts * self.calibration.direction,
        ))
    }
    fn calibration_tick(&mut self, now: u64) -> Option<[u32; 3]> {
        let elapsed = now - self.started;
        let alpha = if elapsed < ALIGN_US {
            0.0
        } else {
            TAU * ((elapsed - ALIGN_US) as f32 / SWEEP_US as f32).min(1.0)
        };
        if elapsed < ALIGN_US {
            self.cal_previous = self.unwrapped;
        } else if self.cal_start.is_none() {
            self.cal_start = Some(self.cal_previous);
        }
        if elapsed >= ALIGN_US + SWEEP_US + SETTLE_US {
            let movement = self.unwrapped - self.cal_start.unwrap_or(self.unwrapped);
            let expected = TAU / POLE_PAIRS;
            if (movement.abs() - expected).abs() > expected * 0.15 {
                self.stop("calibration_movement");
                return None;
            }
            let direction = if movement > 0.0 { 1.0 } else { -1.0 };
            self.calibration = Calibration {
                valid: true,
                direction,
                zero: wrap(-direction * POLE_PAIRS * self.raw),
            };
            self.stop("");
            return None;
        }
        self.commanded_volts = CAL_VOLTS * (elapsed as f32 / 200_000.0).min(1.0);
        Some(angle_duties(alpha, self.commanded_volts))
    }
}
fn wrap(x: f32) -> f32 {
    x - TAU * libm::floorf((x + PI) / TAU)
}

/// Position error with dead zone; no integral term or unbounded output.
fn restoring(error: f32, step: f32) -> f32 {
    let dead = (step * 0.02).min(0.3 * PI / 180.0);
    (error - error.clamp(-dead, dead)) / (step * 0.4)
}
/// Smooth periodic wells: crossing the midpoint pulls toward the next detent.
/// Integer center changes do not change torque, so count hysteresis is separate
/// from the physical force curve. A small smooth dead band avoids idle buzz.
fn detent(error: f32, step: f32) -> f32 {
    let nearest = error - step * libm::floorf(error / step + 0.5);
    let dead = (step * 0.02).min(0.3 * PI / 180.0).max(2.0 * TAU / 4096.0);
    let distance = (nearest.abs() - dead).max(0.0);
    // Multiplying the original sine by a narrow smoothstep created ~3x the
    // nominal stiffness at the dead-band edge. A lightly loaded grip can ring
    // there even when the bare rotor settles. Remap the whole half-well instead:
    // bounded entry stiffness, unchanged full peak.
    let phase = PI * distance / (step * 0.5 - dead);
    let sine = crate::trig::sin(phase);
    -nearest.signum() * sine * (0.25 + 0.75 * sine)
}
fn damping(c: Config, velocity: f32) -> f32 {
    // Voltage per rad/s, scaled with sqrt(stiffness), not with stiffness.
    // 0.01 * strength was far below the damping needed by a light bare rotor.
    // Suppress derivative noise from single AS5600 counts near rest.
    // At the measured 0.7–1.3ms sample spacing a single 12-bit count can
    // resemble ~1rad/s. Do not convert that low-speed uncertainty to alternating
    // drag at detent entry. Position restoring force and the full peak remain.
    let noise = if c.mode == Mode::Detents { 1.0 } else { 0.3 };
    let speed = velocity - velocity.clamp(-noise, noise);
    if speed == 0.0 {
        return 0.0;
    }
    let coefficient = if c.mode == Mode::Spring {
        0.06 * libm::sqrtf(c.strength / 0.4)
    } else {
        0.07 * libm::sqrtf(c.strength / 0.4 * 15.0 / c.step_deg)
    };
    speed * coefficient
}
#[cfg(test)]
fn force(c: Config, x: f32, velocity: f32, center: &mut f32, position: &mut i32) -> f32 {
    force_with_prediction(c, x, velocity, center, position, 0.0)
}
fn force_with_prediction(
    c: Config,
    x: f32,
    velocity: f32,
    center: &mut f32,
    position: &mut i32,
    prediction: f32,
) -> f32 {
    let step = c.step_deg * PI / 180.0;
    let normalized = match c.mode {
        Mode::Free => 0.0,
        Mode::Spring => -x / (PI / 3.0),
        Mode::RatchetForward | Mode::RatchetReverse => {
            let sign = if c.mode == Mode::RatchetForward {
                1.0
            } else {
                -1.0
            };
            let progress = sign * x;
            if progress >= *center + step {
                let clicks = libm::floorf((progress - *center) / step).clamp(0.0, 1000.0) as i32;
                *center += clicks as f32 * step;
                *position = position.saturating_add(clicks);
            }
            let error = progress - *center;
            sign * if error < 0.0 {
                -restoring(error, step) * 2.0
            } else {
                detent(error, step) * 0.40
            }
        }
        Mode::Detents | Mode::Switch | Mode::Selector => {
            let max = match c.mode {
                Mode::Switch => 1,
                Mode::Selector => 4,
                _ => 1_000_000,
            };
            let min = if c.mode == Mode::Detents {
                -1_000_000
            } else {
                0
            };
            let error = x - *center;
            if error > step * 0.60 && *position < max {
                let count = libm::floorf((error - step * 0.60) / step) as i32 + 1;
                let next = position.saturating_add(count).min(max);
                *center += (next - *position) as f32 * step;
                *position = next;
            } else if error < -step * 0.60 && *position > min {
                let count = libm::floorf((-error - step * 0.60) / step) as i32 + 1;
                let next = position.saturating_sub(count).max(min);
                *center += (next - *position) as f32 * step;
                *position = next;
            }
            let error = x - *center;
            let at_end = c.mode != Mode::Detents
                && ((*position == min && error < 0.0) || (*position == max && error > 0.0));
            let restoring_force = if at_end {
                -restoring(error, step) * 2.0
            } else {
                detent(error + prediction, step)
            };
            restoring_force
        }
    };
    if c.mode == Mode::Free {
        return 0.0;
    }
    let near_rest = normalized.abs() < 0.005 && velocity.abs() < 1.0;
    let damping_volts = if near_rest { 0.0 } else { damping(c, velocity) };
    // Fast turns used to consume the whole output range with damping, making
    // every detent feel like uniform drag. Reserve headroom for the detent
    // curve without reducing the user's selected peak voltage. At small
    // velocities the existing damping and center dead band remain unchanged.
    let damping_volts = if c.mode == Mode::Detents {
        damping_volts.clamp(-0.35 * c.strength, 0.35 * c.strength)
    } else {
        damping_volts
    };
    (normalized * c.strength - damping_volts).clamp(-c.strength, c.strength)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::Reading;

    fn legacy_detent(error: f32, step: f32) -> f32 {
        let nearest = error - step * libm::floorf(error / step + 0.5);
        let dead = (step * 0.02).min(0.3 * PI / 180.0).max(2.0 * TAU / 4096.0);
        let blend = ((nearest.abs() - dead) / dead).clamp(0.0, 1.0);
        -crate::trig::sin(TAU * nearest / step) * blend * blend * (3.0 - 2.0 * blend)
    }

    #[test]
    fn recorded_ringing_replay_reduces_positive_feedback_work() {
        extern crate std;
        let bytes = include_bytes!("../tests/data/detent-ringing.hktr");
        assert_eq!(&bytes[..4], b"HKTR");
        let mut frames = std::vec::Vec::new();
        for b in bytes[8..].chunks_exact(16) {
            frames.push((
                u32::from_le_bytes(b[..4].try_into().unwrap()) as u64,
                u16::from_le_bytes(b[4..6].try_into().unwrap()) as u64,
                u16::from_le_bytes(b[6..8].try_into().unwrap()),
                i16::from_le_bytes(b[8..10].try_into().unwrap()) as f32 / 100.0,
                i16::from_le_bytes(b[10..12].try_into().unwrap()) as f32 / 1000.0,
                i16::from_le_bytes(b[12..14].try_into().unwrap()) as f32 / 100.0,
            ));
        }
        let first = frames[0];
        let mut h = Haptics::default();
        h.config.strength = 1.0;
        h.calibration.valid = true;
        h.calibration.direction = -1.0;
        h.start(first.0 - ENGAGE_US);
        h.phase = Phase::Running;
        h.engaged_at = first.0 - ENGAGE_US;
        h.last_update = first.0;
        h.origin = first.2 as f32 * TAU / 4096.0 - first.5 * PI / 180.0;
        h.velocity = first.3;
        let mut output = std::vec::Vec::new();
        for &(at, age, raw, _, _, _) in &frames {
            h.update(
                at,
                Snapshot {
                    reading: Some(Reading::decode(0x33, raw.to_be_bytes())),
                    sampled_at: Some(at - age),
                    error: None,
                },
            );
            assert!(h.active(), "{}", h.error);
            output.push(h.commanded_volts);
        }
        // Integrate the previous applied voltage against measured movement.
        // This is a trajectory replay, not a new closed-loop hardware test.
        let mut old_work = 0.0;
        let mut new_work = 0.0;
        let mut j = 0;
        for pair in frames.windows(2) {
            let a = pair[0];
            let b = pair[1];
            if b.0 - b.1 <= a.0 - a.1 {
                continue;
            }
            let mid = ((a.0 - a.1) + (b.0 - b.1)) / 2;
            if mid < frames[0].0 {
                continue;
            }
            while j + 1 < frames.len() && frames[j + 1].0 <= mid {
                j += 1;
            }
            let delta = wrap((b.2 as f32 - a.2 as f32) * TAU / 4096.0);
            old_work += frames[j].4 * delta;
            new_work += output[j] * delta;
        }
        std::eprintln!("recorded voltage-angle work: old={old_work}, predicted={new_work}");
        assert!(old_work > 0.5);
        assert!(
            new_work < old_work * 0.5,
            "old={old_work} predicted={new_work}"
        );
    }

    #[test]
    fn steady_quantized_turn_does_not_pulse_damping_between_counts() {
        for interval in [1000u64, 1500, 2200] {
            for speed in [-1.0f32, -0.5, -0.25, 0.25, 0.5, 1.0] {
                let mut h = Haptics::default();
                h.config.strength = 1.0;
                h.calibration.valid = true;
                h.start(0);
                h.update(0, snapshot(0, 1.0));
                let mut squared_error = 0.0;
                let mut n = 0;
                for i in 1..1500 {
                    let at = i * interval;
                    h.update(at + 400, snapshot(at, 1.0 + speed * at as f32 * 1e-6));
                    assert!(h.active());
                    if i > 1000 {
                        squared_error += (h.velocity - speed) * (h.velocity - speed);
                        n += 1;
                    }
                }
                let rms = libm::sqrtf(squared_error / n as f32);
                assert!(rms < 0.24, "speed={speed} interval={interval} rms={rms}");
            }
        }
    }

    #[test]
    fn recorded_moving_trace_keeps_fast_snap_but_smooths_slow_count_steps() {
        extern crate std;
        let bytes = include_bytes!("../tests/data/detent-moving-quantization.hktr");
        assert_eq!(&bytes[..8], b"HKTR\x02\x00\x00\x01");
        let mut h = Haptics::default();
        h.config.strength = 1.0;
        h.calibration = Calibration {
            valid: true,
            direction: -1.0,
            zero: 0.0,
        };
        let mut first_at = 0;
        let mut old_prev = 0.0f32;
        let mut new_prev = 0.0f32;
        let mut old_chatter = 0.0f32;
        let mut new_chatter = 0.0f32;
        let mut peak = 0.0f32;
        for (index, b) in bytes[8..].chunks_exact(20).enumerate() {
            let applied = u32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
            let age = u16::from_le_bytes(b[4..6].try_into().unwrap()) as u64;
            let raw = u16::from_le_bytes(b[6..8].try_into().unwrap());
            let old = i16::from_le_bytes(b[10..12].try_into().unwrap()) as f32 / 1000.0;
            let error =
                i16::from_le_bytes(b[12..14].try_into().unwrap()) as f32 / 100.0 * PI / 180.0;
            let calculation = u16::from_le_bytes(b[16..18].try_into().unwrap()) as u64;
            // Control entry precedes snapshot acquisition. If I2C completes
            // during preemption, its sample can be newer than that entry.
            // Use the later bound; exact post-snapshot time is not in v2.
            let entered = (applied - calculation).max(applied - age);
            if index == 0 {
                first_at = applied;
                h.start(entered - ENGAGE_US);
                h.phase = Phase::Running;
                h.engaged_at = entered - ENGAGE_US;
                h.last_update = entered;
                h.origin = raw as f32 * TAU / 4096.0 - error;
            }
            h.update(
                entered,
                Snapshot {
                    reading: Some(Reading::decode(0x33, raw.to_be_bytes())),
                    sampled_at: Some(applied - age),
                    error: None,
                },
            );
            assert!(h.active(), "{}", h.error);
            let step = h.config.step_deg * PI / 180.0;
            let old_residual = old - legacy_detent(error, step);
            let new_residual = h.commanded_volts - detent(error, step);
            if index > 20 && applied - first_at < 200_000 {
                // Remove each curve's intended position force before comparing
                // temporal chatter; the entry shape deliberately changed.
                old_chatter += (old_residual - old_prev).abs();
                new_chatter += (new_residual - new_prev).abs();
            }
            old_prev = old_residual;
            new_prev = new_residual;
            peak = peak.max(h.commanded_volts.abs());
        }
        std::eprintln!(
            "moving slow-segment total variation old={old_chatter} new={new_chatter}; peak={peak}"
        );
        assert!(new_chatter < old_chatter * 0.7);
        assert!(peak > 0.99, "peak was reduced");
        assert!(
            h.fast_feedback_until > 0,
            "snap dynamics never selected fast feedback"
        );
    }

    #[test]
    fn recorded_slow_turn_replay_reduces_force_chatter() {
        extern crate std;
        let bytes = include_bytes!("../tests/data/detent-slow-turn.hktr");
        assert_eq!(&bytes[..8], b"HKTR\x02\x00\x00\x01");
        let mut h = Haptics::default();
        h.config.strength = 1.0;
        h.calibration = Calibration {
            valid: true,
            direction: -1.0,
            zero: 0.0,
        };
        let mut previous_old: Option<f32> = None;
        let mut previous_new: Option<f32> = None;
        let mut old_steps = 0;
        let mut new_steps = 0;
        let mut old_noise = 0.0;
        let mut new_noise = 0.0;
        let mut peak = 0.0f32;
        let mut expected_static_peak = 0.0f32;
        for (index, b) in bytes[8..].chunks_exact(20).enumerate() {
            let applied_at = u32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
            let age = u16::from_le_bytes(b[4..6].try_into().unwrap()) as u64;
            let raw = u16::from_le_bytes(b[6..8].try_into().unwrap());
            let old = i16::from_le_bytes(b[10..12].try_into().unwrap()) as f32 / 1000.0;
            let error =
                i16::from_le_bytes(b[12..14].try_into().unwrap()) as f32 / 100.0 * PI / 180.0;
            let calculation = u16::from_le_bytes(b[16..18].try_into().unwrap()) as u64;
            // v2 timestamps PWM application, after the recorded calculation.
            // Replay the original sample age at computation entry, not at output.
            let entered_at = applied_at - calculation;
            if index == 0 {
                h.start(entered_at - ENGAGE_US);
                h.phase = Phase::Running;
                h.engaged_at = entered_at - ENGAGE_US;
                h.last_update = entered_at;
                h.origin = raw as f32 * TAU / 4096.0 - error;
            }
            h.update(
                entered_at,
                Snapshot {
                    reading: Some(Reading::decode(0x33, raw.to_be_bytes())),
                    sampled_at: Some(applied_at - age),
                    error: None,
                },
            );
            assert!(h.active(), "{}", h.error);
            let new = h.commanded_volts;
            if index > 20 {
                let position_force = detent(error, h.config.step_deg * PI / 180.0);
                let old_position_force = legacy_detent(error, h.config.step_deg * PI / 180.0);
                old_noise += (old - old_position_force).abs();
                new_noise += (new - position_force).abs();
                expected_static_peak = expected_static_peak.max(position_force.abs());
                if previous_old.is_some_and(|v| (old - v).abs() > 0.1) {
                    old_steps += 1;
                }
                if previous_new.is_some_and(|v| (new - v).abs() > 0.1) {
                    new_steps += 1;
                }
                peak = peak.max(new.abs());
            }
            previous_old = Some(old);
            previous_new = Some(new);
        }
        std::eprintln!(
            "slow-turn >0.1V steps old={old_steps} new={new_steps}; derivative noise old={old_noise} new={new_noise}, peak={peak}"
        );
        assert!(old_steps >= 20);
        assert!(new_steps * 3 < old_steps);
        assert!(new_noise < old_noise * 0.5);
        assert!(
            peak >= expected_static_peak - 0.05,
            "output no longer follows the static curve"
        );
    }

    #[test]
    fn angle_quantization_does_not_create_false_velocity_and_drag() {
        let mut h = Haptics::default();
        h.config.strength = 1.0;
        h.calibration = Calibration {
            valid: true,
            direction: 1.0,
            zero: 0.0,
        };
        h.start(0);
        h.update(0, snapshot(0, 1.0));
        let mut low = f32::MAX;
        let mut high = f32::MIN;
        for i in 1..1500 {
            let at = i * 1000;
            let offset = (i as f32 / 200.0).min(1.0) * 3.9 * PI / 180.0;
            let noise = if i % 2 == 0 { 1.0 } else { -1.0 };
            h.update(at + 400, snapshot(at, 1.0 + offset + noise * TAU / 4096.0));
            assert!(h.active());
            if i > 1000 {
                assert!(h.velocity.abs() < 0.02, "false speed {}", h.velocity);
                low = low.min(h.commanded_volts);
                high = high.max(h.commanded_volts);
            }
        }
        // Measured position is still allowed to vary by its one-count resolution;
        // only the false velocity/forecast/drag amplification is removed.
        assert!(high - low < 0.1, "command ripple {}", high - low);
        assert!(high.abs().max(low.abs()) > 0.6, "holding force was reduced");
    }

    #[test]
    fn quantization_tracking_preserves_sustained_slow_and_fast_velocity() {
        for speed in [-10.0, -1.0, -0.1, 0.1, 1.0, 10.0] {
            let mut h = Haptics::default();
            h.calibration.valid = true;
            h.start(0);
            h.update(0, snapshot(0, 1.0));
            for i in 1..2000 {
                let at = i * 1000;
                h.update(at + 400, snapshot(at, 1.0 + speed * i as f32 * 0.001));
            }
            // Compare a multi-sample angle derivative: individual count increments
            // remain quantized at very slow speed, with no long-term scale change.
            let tracked = h.velocity_angle;
            let measured = h.unwrapped;
            assert!((tracked - measured).abs() <= 1.51 * TAU / 4096.0);
            assert_eq!(h.velocity.signum(), speed.signum());
            assert!((h.angle_deg() * PI / 180.0 - speed * 1.999).abs() < 2.0 * TAU / 4096.0);
        }
    }

    #[test]
    fn prediction_compensates_force_without_advancing_measured_detent_count() {
        let c = Config {
            strength: 1.0,
            ..Config::default()
        };
        let step = c.step_deg * PI / 180.0;
        let mut center = 0.0;
        let mut position = 0;
        let volts =
            force_with_prediction(c, step * 0.45, 3.0, &mut center, &mut position, step * 0.2);
        assert!(volts > 0.0 && volts <= 1.0);
        assert_eq!(position, 0);
        assert_eq!(center, 0.0);
    }
    fn snapshot(at: u64, radians: f32) -> Snapshot {
        Snapshot {
            reading: Some(Reading::decode(
                0x33,
                ((radians.rem_euclid(TAU) * 4096.0 / TAU) as u16).to_be_bytes(),
            )),
            sampled_at: Some(at),
            error: None,
        }
    }

    #[test]
    fn fast_turn_keeps_both_sides_of_each_detent_instead_of_uniform_drag() {
        for step_deg in [5.0, 15.0, 30.0, 60.0] {
            for strength in [0.2, 0.4, 1.0] {
                let c = Config {
                    mode: Mode::Detents,
                    strength,
                    step_deg,
                };
                let step = step_deg * PI / 180.0;
                let limit = strength;
                for speed in [-20.0, -10.0, -3.0, 3.0, 10.0, 20.0] {
                    let mut center = 0.0;
                    let mut position = 0;
                    let before = force(c, step * 0.25, speed, &mut center, &mut position);
                    let after = force(c, step * 0.75, speed, &mut center, &mut position);
                    assert!(
                        before < -0.6 * limit && after > 0.6 * limit,
                        "step={step_deg} strength={strength} speed={speed} before={before} after={after}"
                    );
                    // Keep the existing center/noise dead band at low speed.
                    let low_speed = force(c, 0.0, 0.5, &mut center, &mut position);
                    assert_eq!(low_speed, 0.0);
                }
            }
        }
    }
    #[test]
    fn range_and_no_uncalibrated_force() {
        let mut h = Haptics::default();
        assert!(!h.start(0));
        assert!(h.calibrate(0));
        h.stop("");
        assert!(!h.active());
        assert!(h.update(1, snapshot(1, 0.0)).is_none());
        for strength in [f32::NAN, 0.0, 1.01] {
            assert!(
                !Config {
                    strength,
                    ..Config::default()
                }
                .valid()
            );
        }
    }
    #[test]
    fn force_mode_retains_zero_torque_braking_but_free_and_stop_float() {
        let mut h = Haptics::default();
        h.calibration = Calibration {
            valid: true,
            direction: 1.0,
            zero: 0.0,
        };
        h.start(0);
        assert_eq!(
            h.update(0, snapshot(0, 1.0)),
            Some([crate::PWM_PERIOD / 2; 3])
        );
        h.stop("");
        assert_eq!(h.update(1000, snapshot(1000, 1.0)), None);
        h.config.mode = Mode::Free;
        h.start(2000);
        assert_eq!(h.update(2000, snapshot(2000, 1.0)), None);
    }

    #[test]
    fn a_fast_turn_coasts_without_exiting_and_recovers_when_slow() {
        let mut h = Haptics::default();
        h.calibration = Calibration {
            valid: true,
            direction: 1.0,
            zero: 0.0,
        };
        assert!(h.start(0));
        h.update(0, snapshot(0, 1.0));
        // 0.4 rad in 5ms = 80 rad/s: unload immediately, stay in mode.
        assert!(h.update(5_000, snapshot(5_000, 1.4)).is_none());
        assert!(h.active() && h.coasting);
        assert_eq!(h.commanded_volts, 0.0);
        assert_eq!(h.phase(), "coasting");
        assert_eq!(h.error, "");
        for t in (6_000..=180_000).step_by(1000) {
            h.update(t, snapshot(t, 1.4));
        }
        assert!(h.active() && !h.coasting);
        assert_eq!(h.error, "");
        // A normal quick hand movement above the former 15 rad/s limit
        // must neither terminate nor unload the session.
        h.stop("");
        h.start(200_000);
        h.update(200_000, snapshot(200_000, 1.0));
        h.update(205_000, snapshot(205_000, 1.1));
        assert!(h.active() && !h.coasting);
        assert_eq!(h.error, "");
    }
    #[test]
    fn calibration_accepts_both_sensor_directions_and_weak_warning() {
        for direction in [-1.0, 1.0] {
            let mut h = Haptics::default();
            assert!(h.calibrate(0));
            for t in (0u64..=4_510_000).step_by(500) {
                let a = TAU * ((t.saturating_sub(ALIGN_US)) as f32 / SWEEP_US as f32).min(1.0);
                h.update(t, snapshot(t, 1.0 + direction * a / POLE_PAIRS));
            }
            assert!(h.calibrated(), "{}", h.error);
            assert!(!h.active());
            assert_eq!(h.sensor_direction(), direction as i32);
        }
    }
    #[test]
    fn calibration_stall_and_sensor_loss_never_enable_modes() {
        let mut h = Haptics::default();
        h.calibrate(0);
        for t in (0u64..=4_510_000).step_by(500) {
            h.update(t, snapshot(t, 1.0));
        }
        assert!(!h.calibrated());
        assert_eq!(h.error, "calibration_movement");
        h.calibrate(5_000_000);
        h.update(5_000_000, snapshot(5_000_000, 1.0));
        h.update(5_001_000, Snapshot::default());
        assert!(!h.active());
    }
    #[test]
    fn sensor_wrap_is_continuous() {
        let mut h = Haptics::default();
        assert!(h.sample(0, snapshot(0, TAU - 0.01)));
        assert!(h.sample(5_000, snapshot(5_000, 0.01)));
        assert!((h.unwrapped - (TAU + 0.01)).abs() < 0.003);
    }
    #[test]
    fn detents_snap_with_hysteresis_and_selector_bounds() {
        let mut center = 0.0;
        let mut pos = 0;
        let c = Config::default();
        let s = c.step_deg * PI / 180.0;
        force(c, s * 0.59, 0.0, &mut center, &mut pos);
        assert_eq!(pos, 0);
        force(c, s * 0.61, 0.0, &mut center, &mut pos);
        assert_eq!(pos, 1);
        force(c, s * 0.45, 0.0, &mut center, &mut pos);
        assert_eq!(pos, 1);
        let c = Config {
            mode: Mode::Selector,
            ..c
        };
        assert!(force(c, 100.0, 0.0, &mut center, &mut pos) < 0.0);
        assert_eq!(pos, 4);
        assert!(force(c, -100.0, 0.0, &mut center, &mut pos) > 0.0);
        assert_eq!(pos, 0);
    }
    #[test]
    fn detent_wells_pull_to_the_nearest_center_without_a_snap_discontinuity() {
        let c = Config {
            strength: 0.4,
            ..Config::default()
        };
        let step = c.step_deg * PI / 180.0;
        let mut center = 0.0;
        let mut pos = 0;
        // The remapped well reaches full force half a dead-band width beyond
        // a quarter step. Verify the unchanged peak at its actual location.
        let dead = (step * 0.02).min(0.3 * PI / 180.0).max(2.0 * TAU / 4096.0);
        let peak = step * 0.25 + dead * 0.5;
        assert!(force(c, peak, 0.0, &mut center, &mut pos) < -0.399);
        assert!(force(c, step - peak, 0.0, &mut center, &mut pos) > 0.399);
        let before = force(c, step * 0.5999, 0.0, &mut center, &mut pos);
        let after = force(c, step * 0.6001, 0.0, &mut center, &mut pos);
        assert!((before - after).abs() < 0.003);
        assert_eq!(force(c, step, 0.0, &mut center, &mut pos), 0.0);
        // Bounded modes must push back even several steps outside an end stop;
        // they must never create extra periodic wells beyond the allowed range.
        for mode in [Mode::Switch, Mode::Selector] {
            let c = Config { mode, ..c };
            center = 0.0;
            pos = 0;
            assert!(force(c, -step * 2.75, 0.0, &mut center, &mut pos) > 0.39);
            assert!(force(c, step * 20.75, 0.0, &mut center, &mut pos) < -0.39);
        }
    }
    #[test]
    fn ratchet_clicks_are_distinct_and_both_directions_mirror() {
        let step = 30.0 * PI / 180.0;
        for (mode, sign) in [(Mode::RatchetForward, 1.0), (Mode::RatchetReverse, -1.0)] {
            let c = Config {
                mode,
                strength: 1.0,
                step_deg: 30.0,
            };
            let mut center = 0.0;
            let mut pos = 0;
            assert!(
                (force(c, sign * step * 0.25, 0.0, &mut center, &mut pos) + sign * 0.4).abs()
                    < 0.001
            );
            assert!(
                (force(c, sign * step * 0.75, 0.0, &mut center, &mut pos) - sign * 0.4).abs()
                    < 0.001
            );
            force(c, sign * step * 1.05, 0.0, &mut center, &mut pos);
            assert_eq!(pos, 1);
            assert!(sign * force(c, sign * step * 0.75, 0.0, &mut center, &mut pos) > 0.9);
        }
    }
    #[test]
    fn feedback_reverses_on_the_next_sample_and_only_engagement_is_ramped() {
        for direction in [-1.0, 1.0] {
            let mut h = Haptics::default();
            h.config.strength = 1.0;
            h.config.step_deg = 30.0;
            h.calibration = Calibration {
                valid: true,
                direction,
                zero: 0.3,
            };
            h.start(0);
            let origin = 1.0;
            for t in (0..=200_000).step_by(5000) {
                h.update(t, snapshot(t, origin));
            }
            let step = h.config.step_deg * PI / 180.0;
            h.update(205_000, snapshot(205_000, origin + step * 0.25));
            // Isolate the static force-sign response from velocity damping.
            h.velocity = 0.0;
            h.update(206_000, snapshot(205_000, origin + step * 0.25));
            assert!(h.commanded_volts < -0.9);
            h.update(210_000, snapshot(210_000, origin + step * 0.75));
            h.velocity = 0.0;
            let pwm = h
                .update(211_000, snapshot(210_000, origin + step * 0.75))
                .unwrap();
            assert!(
                h.commanded_volts > 0.7,
                "must not retain the previous torque sign"
            );
            // Reconstruct the stator vector. Its cross product with the aligned
            // rotor axis, corrected for sensor direction, must agree with force.
            let a = pwm[0] as f32 - (pwm[1] + pwm[2]) as f32 / 2.0;
            let b = (pwm[1] as f32 - pwm[2] as f32) * libm::sqrtf(3.0) / 2.0;
            let align = direction * POLE_PAIRS * h.raw + h.calibration.zero;
            let torque = (libm::sinf(align) * b + libm::cosf(align) * a) * direction;
            assert!(torque > 0.0);
            h.stop("");
            h.start(300_000);
            h.update(300_000, snapshot(300_000, origin));
            h.update(305_000, snapshot(305_000, origin + step * 0.25));
            h.velocity = 0.0;
            h.update(306_000, snapshot(305_000, origin + step * 0.25));
            assert!(h.commanded_volts < 0.0 && h.commanded_volts.abs() < 0.04);
        }
    }
    #[test]
    fn spring_and_ratchets_oppose_displacement_and_never_exceed_voltage() {
        for mode in [
            Mode::Free,
            Mode::Detents,
            Mode::Switch,
            Mode::Selector,
            Mode::Spring,
            Mode::RatchetForward,
            Mode::RatchetReverse,
        ] {
            let c = Config {
                mode,
                ..Config::default()
            };
            let mut center = 0.0;
            let mut pos = 0;
            for i in -1000..1000 {
                let v = force(c, i as f32 * 0.01, 2.0, &mut center, &mut pos);
                assert!(v.is_finite() && v.abs() <= c.strength);
            }
        }
        let mut center = 0.0;
        let mut pos = 0;
        assert!(
            force(
                Config {
                    mode: Mode::Spring,
                    ..Config::default()
                },
                0.2,
                0.0,
                &mut center,
                &mut pos
            ) < 0.0
        );
        assert!(
            force(
                Config {
                    mode: Mode::RatchetForward,
                    ..Config::default()
                },
                -0.2,
                0.0,
                &mut center,
                &mut pos
            ) > 0.0
        );
        assert!(
            force(
                Config {
                    mode: Mode::RatchetReverse,
                    ..Config::default()
                },
                0.2,
                0.0,
                &mut center,
                &mut pos
            ) < 0.0
        );
    }
    #[test]
    fn recorded_touch_entry_keeps_damping_below_position_restoring_force() {
        extern crate std;
        let bytes = include_bytes!("../tests/data/detent-entry-touch.hktr");
        assert_eq!(&bytes[..8], b"HKTR\x02\x00\x00\x01");
        let mut h = Haptics::default();
        h.config.strength = 1.0;
        h.calibration = Calibration {
            valid: true,
            direction: -1.0,
            zero: 0.0,
        };
        let mut old_noise = 0.0f32;
        let mut new_noise = 0.0f32;
        let mut old_opposing = 0;
        let mut new_opposing = 0;
        let mut samples = 0;
        for (index, b) in bytes[8..].chunks_exact(20).enumerate() {
            let at = u32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
            let age = u16::from_le_bytes(b[4..6].try_into().unwrap()) as u64;
            let raw = u16::from_le_bytes(b[6..8].try_into().unwrap());
            let velocity = i16::from_le_bytes(b[8..10].try_into().unwrap()) as f32 / 100.0;
            let old = i16::from_le_bytes(b[10..12].try_into().unwrap()) as f32 / 1000.0;
            let degrees = i16::from_le_bytes(b[12..14].try_into().unwrap()) as f32 / 100.0;
            let error = degrees * PI / 180.0;
            let calc = u16::from_le_bytes(b[16..18].try_into().unwrap()) as u64;
            let entered = (at - calc).max(at - age);
            if index == 0 {
                h.start(entered - ENGAGE_US);
                h.phase = Phase::Running;
                h.engaged_at = entered - ENGAGE_US;
                h.last_update = entered;
                h.origin = raw as f32 * TAU / 4096.0 - error;
            }
            h.update(
                entered,
                Snapshot {
                    reading: Some(Reading::decode(0x33, raw.to_be_bytes())),
                    sampled_at: Some(at - age),
                    error: None,
                },
            );
            assert!(h.active(), "{}", h.error);
            if index > 20 && degrees.abs() > 0.35 && degrees.abs() < 1.2 && velocity.abs() < 0.9 {
                let restoring = detent(error, h.config.step_deg * PI / 180.0);
                old_noise += (old - restoring).abs();
                new_noise += (h.commanded_volts - restoring).abs();
                old_opposing += u32::from(old * degrees > 0.0001);
                new_opposing += u32::from(h.commanded_volts * degrees > 0.0001);
                samples += 1;
            }
        }
        std::eprintln!(
            "entry samples={samples} residual old={old_noise} new={new_noise}; damping-over-restoring old={old_opposing} new={new_opposing}"
        );
        assert!(samples > 200);
        assert!(old_opposing >= 3);
        assert_eq!(new_opposing, 0);
        assert!(new_noise < old_noise);
        // Residual includes legitimate phase prediction and rounded trace angle;
        // require its mean below half a PWM count, rather than inventing a
        // percentage reduction below the measurement/command resolution.
        assert!(
            new_noise / (samples as f32) < crate::SUPPLY_VOLTS / crate::PWM_PERIOD as f32 * 0.5
        );
    }

    #[test]
    fn recorded_entry_position_steps_are_smoothed_without_cutting_holding_force() {
        extern crate std;
        let bytes = include_bytes!("../tests/data/detent-entry-position-steps.hktr");
        assert_eq!(&bytes[..8], b"HKTR\x02\x00\x00\x01");
        let mut h = Haptics::default();
        h.config.strength = 1.0;
        h.calibration = Calibration {
            valid: true,
            direction: -1.0,
            zero: 0.0,
        };
        let mut previous: Option<(f32, f32)> = None;
        let mut old_step = 0.0f32;
        let mut new_step = 0.0f32;
        let mut old_variation = 0.0f32;
        let mut new_variation = 0.0f32;
        let mut samples = 0;
        for (i, b) in bytes[8..].chunks_exact(20).enumerate() {
            let at = u32::from_le_bytes(b[..4].try_into().unwrap()) as u64;
            let age = u16::from_le_bytes(b[4..6].try_into().unwrap()) as u64;
            let raw = u16::from_le_bytes(b[6..8].try_into().unwrap());
            let error =
                i16::from_le_bytes(b[12..14].try_into().unwrap()) as f32 / 100.0 * PI / 180.0;
            let calc = u16::from_le_bytes(b[16..18].try_into().unwrap()) as u64;
            let entered = (at - calc).max(at - age);
            if i == 0 {
                h.start(entered - ENGAGE_US);
                h.phase = Phase::Running;
                h.engaged_at = entered - ENGAGE_US;
                h.last_update = entered;
                h.origin = raw as f32 * TAU / 4096.0 - error;
            }
            h.update(
                entered,
                Snapshot {
                    reading: Some(Reading::decode(0x33, raw.to_be_bytes())),
                    sampled_at: Some(at - age),
                    error: None,
                },
            );
            assert!(h.active(), "{}", h.error);
            // The previous version differed only in using raw position for
            // force. Compare identical samples/velocity/prediction, not an
            // older force curve or a different strength setting.
            let prediction =
                h.velocity * ((entered - h.last_sample.unwrap() + 500).min(2500) as f32 * 1e-6);
            let mut center = h.center;
            let mut position = h.position;
            let direct = force_with_prediction(
                h.config,
                h.unwrapped - h.origin,
                h.velocity,
                &mut center,
                &mut position,
                prediction,
            );
            let smooth = h.commanded_volts;
            if let Some((old, new)) = previous {
                if i > 20 {
                    old_step = old_step.max((direct - old).abs());
                    new_step = new_step.max((smooth - new).abs());
                    old_variation += (direct - old).abs();
                    new_variation += (smooth - new).abs();
                    samples += 1;
                }
            }
            previous = Some((direct, smooth));
        }
        std::eprintln!(
            "entry position steps: peak old={old_step} new={new_step}; variation old={old_variation} new={new_variation}"
        );
        assert!(samples > 200);
        assert!(
            old_step > 0.025,
            "fixture no longer contains position steps"
        );
        assert!(new_step < old_step * 0.85);
        assert!(new_variation < old_variation * 0.85);
        // At a fixed displacement the filter must converge to the original
        // force, rather than disguising vibration by lowering the strength.
        for i in 0..20 {
            let at = h.last_update + 1500;
            h.update(at, snapshot(at, h.raw));
            if i == 19 {
                assert!((h.force_angle - h.unwrapped).abs() < TAU / 4096.0 * 0.01);
            }
        }
    }

    #[test]
    fn position_smoothing_bypasses_fast_motion_and_preserves_measured_counts() {
        let mut h = Haptics::default();
        h.calibration.valid = true;
        h.config.strength = 1.0;
        h.start(0);
        h.update(0, snapshot(0, 1.0));
        for i in 1..300 {
            let at = i * 1000;
            h.update(at, snapshot(at, 1.0 + i as f32 * 0.01));
            assert!(h.active());
            assert!(h.fast_feedback_until > at);
            assert_eq!(h.force_angle, h.unwrapped);
        }
        assert!(
            h.position > 5,
            "smoothed force altered measured count tracking"
        );
    }

    #[test]
    fn sub_count_velocity_uncertainty_does_not_alternate_entry_drag() {
        let c = Config {
            strength: 1.0,
            ..Config::default()
        };
        for degrees in [-1.0f32, -0.5, 0.5, 1.0] {
            let x = degrees * PI / 180.0;
            let baseline = force(c, x, 0.0, &mut 0.0, &mut 0);
            for velocity in [-1.0, -0.8, -0.4, 0.4, 0.8, 1.0] {
                let output = force(c, x, velocity, &mut 0.0, &mut 0);
                assert!((output - baseline).abs() < 1e-6, "x={degrees} v={velocity}");
            }
            assert!(baseline.abs() > 0.01, "position restoring force vanished");
        }
        assert!(damping(c, 3.0) > 0.2, "fast-motion damping was removed");
    }

    #[test]
    fn detent_entry_stiffness_does_not_spike_at_the_dead_band_edge() {
        for degrees in [5.0, 15.0, 30.0, 60.0, 90.0] {
            let step = degrees * PI / 180.0;
            let dx = step / 20_000.0;
            let mut peak = 0.0f32;
            let mut max_slope = 0.0f32;
            for i in 0..10_000 {
                let x = i as f32 * dx;
                peak = peak.max(detent(x, step).abs());
                max_slope = max_slope
                    .max(((detent(x + dx, step) - detent(x - dx, step)) / (2.0 * dx)).abs());
            }
            assert!(peak > 0.999, "peak lost at {degrees}");
            assert!(
                max_slope < 1.12 * TAU / step,
                "step={degrees} entry slope={max_slope}, plain={}",
                TAU / step
            );
        }
    }

    #[test]
    fn gently_loaded_grip_does_not_buzz_at_detent_entry() {
        extern crate std;
        let mut failures = 0;
        for stiffness in [0.1, 0.3, 1.0] {
            for damping in [0.0002, 0.0007] {
                for degrees in [0.45, 0.75, 1.0] {
                    let mut h = Haptics::default();
                    h.config.strength = 1.0;
                    h.calibration = Calibration {
                        valid: true,
                        direction: 1.0,
                        zero: 0.0,
                    };
                    h.start(0);
                    let origin = 1.0;
                    let mut theta = origin;
                    let mut speed = 0.0;
                    let mut history = [origin; 8];
                    let mut sample = snapshot(0, origin);
                    let mut pwm = [crate::PWM_PERIOD / 2; 3];
                    let mut va = 0.0;
                    let mut vb = 0.0;
                    let mut lo = f32::MAX;
                    let mut hi = f32::MIN;
                    let mut sum = 0.0;
                    let mut sum2 = 0.0;
                    let mut n = 0.0;
                    for i in 0..20_000 {
                        let t = i as u64 * 100;
                        let measured = history[i % 8];
                        history[i % 8] = theta;
                        if i % 10 == 5 {
                            let noise = if (i / 10) % 4 == 0 { 1.0 } else { -1.0 };
                            sample = snapshot(t, measured + noise * TAU / 4096.0);
                        }
                        if i % 10 == 0 && (i / 10) % 53 != 1 {
                            pwm = h.update(t, sample).unwrap_or([crate::PWM_PERIOD / 2; 3]);
                            assert!(h.active(), "{}", h.error);
                        }
                        let scale = crate::SUPPLY_VOLTS / crate::PWM_PERIOD as f32 * 2.0 / 3.0;
                        let target_a = (pwm[0] as f32 - (pwm[1] + pwm[2]) as f32 / 2.0) * scale;
                        let target_b =
                            (pwm[1] as f32 - pwm[2] as f32) * libm::sqrtf(3.0) / 2.0 * scale;
                        va += (target_a - va) * (0.0001 / 0.00045);
                        vb += (target_b - vb) * (0.0001 / 0.00045);
                        let hand = origin
                            + degrees * PI / 180.0
                                * ((t.saturating_sub(250_000) as f32 / 250_000.0).min(1.0));
                        let angle = POLE_PAIRS * theta;
                        let q = libm::sinf(angle) * vb + libm::cosf(angle) * va;
                        let torque = (q - 0.0434 * speed) * (0.0434 / 2.55)
                            + stiffness * (hand - theta)
                            - damping * speed;
                        speed += torque / 0.0000037 * 0.0001;
                        theta += speed * 0.0001;
                        if t > 1_000_000 {
                            lo = lo.min(theta);
                            hi = hi.max(theta);
                            sum += h.commanded_volts;
                            sum2 += h.commanded_volts * h.commanded_volts;
                            n += 1.0;
                        }
                    }
                    let std = libm::sqrtf((sum2 / n - (sum / n) * (sum / n)).max(0.0));
                    if hi - lo > 0.003 || std > 0.04 {
                        std::eprintln!(
                            "grip k={stiffness} d={damping} hand={degrees} range={}deg voltage_std={std}",
                            (hi - lo) * 180.0 / PI
                        );
                        failures += 1;
                    }
                }
            }
        }
        assert_eq!(failures, 0, "contacted-grip buzz cases");
    }

    /// Approximate bare 2804 plant using the supplied 3700 g.mm² inertia,
    /// 0.0434 Nm/A torque constant and 2.55 ohm phase resistance. This is a
    /// regression model for release oscillation, not a measured hardware model.
    #[test]
    fn common_detent_spacings_settle_with_quantized_delayed_feedback() {
        extern crate std;
        let mut unstable = 0;
        for release_speed in [0.0, 3.0, 10.0] {
            for delay in [3, 8] {
                for strength in [0.2, 0.4, 1.0] {
                    // Full-voltage 5-degree wells are intentionally not claimed stable:
                    // this approximate model oscillates at high strength/latency.
                    // The API allows them and never silently reduces their voltage.
                    for step_deg in [15.0, 30.0, 60.0] {
                        let mut h = Haptics::default();
                        h.config = Config {
                            mode: Mode::Detents,
                            strength,
                            step_deg,
                        };
                        h.calibration = Calibration {
                            valid: true,
                            direction: 1.0,
                            zero: 0.0,
                        };
                        h.start(0);
                        let origin = 1.0;
                        let release = step_deg * PI / 180.0 * 0.25;
                        let mut theta = origin;
                        let mut speed = 0.0;
                        let mut history = [origin; 8]; // 0.3–0.8ms sensor/transfer delay
                        let mut sample = snapshot(0, origin);
                        let mut pwm = [crate::PWM_PERIOD / 2; 3];
                        let mut va = 0.0;
                        let mut vb = 0.0;
                        let mut lo = f32::MAX;
                        let mut hi = f32::MIN;
                        for i in 0..15_000 {
                            let t = i as u64 * 100;
                            let old = history[i % delay];
                            history[i % delay] = theta;
                            // Sampling and force updates are deliberately out of phase.
                            if i % 10 == 5 {
                                // The sample phase is i % 10 == 5: i % 40 can
                                // never be zero there. Vary by sample index so
                                // this exercises noise, not a constant offset.
                                let noise = if (i / 10) % 4 == 0 { 1.0 } else { -1.0 };
                                sample = snapshot(t, old + noise * TAU / 4096.0);
                            }
                            // Include a 6ms gap: current hardware has observed up to 5.8ms.
                            if i % 10 == 0 && !(1..6).contains(&((i / 10) % 71)) {
                                pwm = h.update(t, sample).unwrap_or([crate::PWM_PERIOD / 2; 3]);
                                assert!(h.active(), "unexpected stop: {}", h.error);
                            }
                            let scale = crate::SUPPLY_VOLTS / crate::PWM_PERIOD as f32 * 2.0 / 3.0;
                            let target_a = (pwm[0] as f32 - (pwm[1] + pwm[2]) as f32 / 2.0) * scale;
                            let target_b =
                                (pwm[1] as f32 - pwm[2] as f32) * libm::sqrtf(3.0) / 2.0 * scale;
                            // Approximate winding L/R dynamics (0.9mH / 2.55 ohm).
                            va += (target_a - va) * (0.0001 / 0.00045);
                            vb += (target_b - vb) * (0.0001 / 0.00045);
                            if t < 250_000 {
                                theta = origin;
                                speed = 0.0;
                            } else if t < 400_000 {
                                // A hand slowly moves a quarter detent, then lets go.
                                let progress = (t - 250_000) as f32 / 150_000.0;
                                theta = origin
                                    + release * progress
                                    + 0.075 * release_speed * progress * progress;
                                speed = release_speed * progress;
                            } else {
                                let a = POLE_PAIRS * theta;
                                let q = libm::sinf(a) * vb + libm::cosf(a) * va;
                                // Voltage mode also produces passive back-EMF damping.
                                let torque =
                                    (q - 0.0434 * speed) * (0.0434 / 2.55) - speed * 0.00001;
                                speed += torque / 0.0000037 * 0.0001;
                                theta += speed * 0.0001;
                            }
                            if t > 1_000_000 {
                                lo = lo.min(theta);
                                hi = hi.max(theta);
                            }
                        }
                        let nearest = libm::roundf((theta - origin) / (step_deg * PI / 180.0));
                        let final_offset = theta - origin - nearest * (step_deg * PI / 180.0);
                        if final_offset.abs() >= 0.02
                            || hi - lo >= 0.02
                            || (release_speed == 0.0 && h.position != 0)
                        {
                            std::eprintln!(
                                "release_speed={release_speed} delay={delay} strength={strength} step={step_deg} offset={} oscillation={} position={}",
                                theta - origin,
                                hi - lo,
                                h.position
                            );
                            unstable += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(unstable, 0, "unstable model cases");
    }
    #[test]
    fn autonomous_haptics_continue_without_network_but_stop_on_bad_feedback() {
        let mut h = Haptics::default();
        h.config.mode = Mode::Free;
        h.start(0);
        h.update(0, snapshot(0, 0.0));
        for t in (500..=800_000).step_by(500) {
            h.update(t, snapshot(t, 0.0));
        }
        assert!(h.active());
        assert_eq!(h.error, "");
        h.stop("");
        assert!(!h.active());
        h.start(1_000_000);
        h.update(1_000_000, snapshot(1_000_000, 0.0));
        h.update(1_020_001, snapshot(1_020_001, 0.0));
        assert_eq!(h.error, "control_gap");
        h.start(2_000_000);
        h.update(2_000_000, snapshot(2_000_000, 0.0));
        h.update(2_001_000, snapshot(1_900_000, 0.0));
        assert!(!h.active());
    }
}
