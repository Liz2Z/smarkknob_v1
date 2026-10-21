#![no_std]

use core::f32::consts::TAU;

pub mod api;
pub mod decimal;
pub mod encoder;
pub mod haptic;
pub mod trace;
mod trig;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parameters {
    pub voltage: f32,
    pub frequency_hz: f32,
}

impl Default for Parameters {
    fn default() -> Self {
        Self {
            voltage: CONTINUOUS_VOLTS,
            frequency_hz: 1_000_000.0 / CONTINUOUS_PERIOD_US as f32,
        }
    }
}

impl Parameters {
    pub fn valid(self) -> bool {
        self.voltage.is_finite()
            && self.frequency_hz.is_finite()
            && (0.5..=2.0).contains(&self.voltage)
            && (0.0..=100.0).contains(&self.frequency_hz)
    }
}

pub const SUPPLY_VOLTS: f32 = 12.0;
pub const TEST_VOLTS: f32 = 12.0;
pub const CONTINUOUS_VOLTS: f32 = 1.0;
pub const KEEPALIVE_US: u64 = 500_000;
pub const TEST_US: u64 = 1_000_000;
pub const RAMP_US: u64 = 200_000;
pub const ALIGN_US: u64 = 200_000;
pub const PWM_PERIOD: u32 = 1 << 11;
pub const MAX_UPDATE_GAP_US: u64 = 20_000;
const FREQUENCY_SLEW_HZ_PER_SECOND: f32 = 50.0;
const ELECTRICAL_PERIOD_US: u64 = 2_000_000;
const CONTINUOUS_PERIOD_US: u64 = 125_000;

/// Three-phase sinusoidal voltage PWM. This is open-loop commutation, not FOC.
/// TEST_VOLTS is the requested amplitude before clipping to the DC bus.
/// Above half the supply, clipping prevents a full requested sine output.
pub fn duties(elapsed_us: u64, reverse: bool) -> Option<[u32; 3]> {
    if elapsed_us >= TEST_US {
        return None;
    }
    let ramp = elapsed_us.min(TEST_US - elapsed_us).min(RAMP_US) as f32 / RAMP_US as f32;
    Some(phase_duties(
        elapsed_us,
        reverse,
        TEST_VOLTS * ramp,
        ELECTRICAL_PERIOD_US,
    ))
}

pub fn continuous_duties(elapsed_us: u64, reverse: bool) -> [u32; 3] {
    let ramp = elapsed_us.min(RAMP_US) as f32 / RAMP_US as f32;
    phase_duties(
        elapsed_us,
        reverse,
        CONTINUOUS_VOLTS * ramp,
        CONTINUOUS_PERIOD_US,
    )
}

fn phase_duties(elapsed_us: u64, reverse: bool, volts: f32, period_us: u64) -> [u32; 3] {
    let sign = if reverse { -1.0 } else { 1.0 };
    // Build a stationary field first so a resting rotor need not catch an
    // already-moving field while the available torque is still ramping up.
    let rotating_us = elapsed_us.saturating_sub(ALIGN_US) % period_us;
    let angle = sign * TAU * rotating_us as f32 / period_us as f32;
    angle_duties(angle, volts)
}

fn angle_duties(angle: f32, volts: f32) -> [u32; 3] {
    // Equal phase duties are exact at zero voltage. Avoid three software sine
    // evaluations on the C3 while a haptic well is at rest; the bridge remains
    // enabled by the caller, retaining the existing passive damping behavior.
    if volts == 0.0 {
        return [PWM_PERIOD / 2; 3];
    }
    let amplitude = volts / SUPPLY_VOLTS;
    [0.0, -TAU / 3.0, TAU / 3.0].map(|offset| {
        let duty = (0.5 + amplitude * trig::sin(angle + offset)) * PWM_PERIOD as f32;
        // Never pass negative or above-period compare values to LEDC.
        (duty + 0.5).clamp(0.0, (PWM_PERIOD - 1) as f32) as u32
    })
}

#[derive(Default)]
pub struct Run {
    started: Option<u64>,
    last_update: u64,
    reverse: bool,
    configured_reverse: bool,
    heartbeat: Option<u64>,
    autonomous: bool,
    parameters: Parameters,
    angle: f32,
    applied_volts: f32,
    applied_hz: f32,
}

impl Run {
    /// Starting an already-running test must not extend its deadline.
    pub fn start(&mut self, now_us: u64, reverse: bool) -> bool {
        if self.started.is_some() {
            return false;
        }
        self.started = Some(now_us);
        self.last_update = now_us;
        self.reverse = reverse;
        self.heartbeat = None;
        self.autonomous = false;
        self.angle = 0.0;
        self.applied_volts = 0.0;
        self.applied_hz = 0.0;
        true
    }

    pub fn start_continuous(&mut self, now_us: u64) -> bool {
        if !self.start(now_us, self.configured_reverse) {
            return false;
        }
        self.heartbeat = Some(now_us);
        true
    }

    /// Board-owned operation: network clients configure it, not sustain it.
    /// Control-gap, explicit-stop and caller-owned duration limits still apply.
    pub fn start_autonomous(&mut self, now_us: u64) -> bool {
        if !self.start_continuous(now_us) {
            return false;
        }
        self.autonomous = true;
        true
    }

    pub fn keep_alive(&mut self, now_us: u64) -> bool {
        if self.autonomous {
            return self.active();
        }
        let Some(last) = self.heartbeat else {
            return false;
        };
        if self.started.is_none() || now_us < last || now_us - last >= KEEPALIVE_US {
            self.stop();
            return false;
        }
        self.heartbeat = Some(now_us);
        true
    }

    pub fn stop(&mut self) {
        self.started = None;
        self.heartbeat = None;
    }

    pub fn active(&self) -> bool {
        self.started.is_some()
    }

    pub fn elapsed_us(&self, now_us: u64) -> Option<u64> {
        self.started.map(|started| now_us.saturating_sub(started))
    }

    pub fn is_continuous(&self) -> bool {
        self.started.is_some() && self.heartbeat.is_some()
    }

    pub fn parameters(&self) -> Parameters {
        self.parameters
    }

    pub fn reverse_setting(&self) -> bool {
        self.configured_reverse
    }

    /// A direction change disarms the run; callers must also disable the bridge.
    /// Returns true only when the direction changed. Never restarts a run.
    pub fn configure_direction(&mut self, reverse: bool) -> bool {
        if self.configured_reverse == reverse {
            return false;
        }
        self.stop();
        self.configured_reverse = reverse;
        true
    }

    /// Target changes preserve electrical phase and slew the voltage on ticks.
    pub fn configure(&mut self, parameters: Parameters) -> bool {
        if !parameters.valid() {
            return false;
        }
        self.parameters = parameters;
        true
    }

    pub fn update(&mut self, now_us: u64) -> Option<[u32; 3]> {
        let started = self.started?;
        if now_us < self.last_update || now_us - self.last_update > MAX_UPDATE_GAP_US {
            self.stop();
            return None;
        }
        let previous_update = self.last_update;
        self.last_update = now_us;
        if let Some(heartbeat) = self.heartbeat {
            if !self.autonomous && (now_us < heartbeat || now_us - heartbeat >= KEEPALIVE_US) {
                self.stop();
                return None;
            }
            let elapsed = now_us - started;
            if elapsed > ALIGN_US {
                let dt = now_us - previous_update.max(started + ALIGN_US);
                let sign = if self.reverse { -1.0 } else { 1.0 };
                let previous_hz = self.applied_hz;
                let max_change = FREQUENCY_SLEW_HZ_PER_SECOND * dt as f32 / 1_000_000.0;
                self.applied_hz +=
                    (self.parameters.frequency_hz - self.applied_hz).clamp(-max_change, max_change);
                self.angle = (self.angle
                    + sign * TAU * (previous_hz + self.applied_hz) * 0.5 * dt as f32 / 1_000_000.0)
                    % TAU;
            }
            let max_change = (now_us - previous_update) as f32 / RAMP_US as f32;
            self.applied_volts +=
                (self.parameters.voltage - self.applied_volts).clamp(-max_change, max_change);
            return Some(angle_duties(self.angle, self.applied_volts));
        }
        let result = duties(now_us - started, self.reverse);
        if result.is_none() {
            self.stop();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomous_run_survives_no_client_and_still_obeys_stop_and_control_gap() {
        let mut run = Run::default();
        assert!(run.start_autonomous(0));
        for now in (0..=3_000_000).step_by(1_000) {
            assert!(run.update(now).is_some());
        }
        assert!(run.active() && run.is_continuous());
        assert!(!run.start_autonomous(3_000_000));
        run.stop();
        assert_eq!(run.update(3_001_000), None);
        run.start_autonomous(4_000_000);
        assert_eq!(run.update(4_020_001), None);
        // The existing USB continuous command still opts into its own lease.
        run.start_continuous(5_000_000);
        for now in (5_000_000..5_500_000).step_by(1_000) {
            assert!(run.update(now).is_some());
        }
        assert_eq!(run.update(5_500_000), None);
    }

    #[test]
    fn idle_until_command_and_explicit_stop() {
        let mut run = Run::default();
        assert_eq!(run.update(0), None);
        assert!(run.start(100, false));
        assert_eq!(run.update(100), Some([1024; 3]));
        run.stop();
        assert_eq!(run.update(101), None);
    }

    #[test]
    fn no_command_can_extend_running_test() {
        let mut run = Run::default();
        run.start(0, false);
        for now in (0..TEST_US).step_by(1_000) {
            assert!(run.update(now).is_some());
            assert!(!run.start(now, true));
        }
        assert_eq!(run.update(TEST_US), None);
        assert!(!run.active());
    }

    #[test]
    fn scheduler_stall_or_clock_regression_disables_output() {
        let mut run = Run::default();
        run.start(10, false);
        assert_eq!(run.update(10 + MAX_UPDATE_GAP_US + 1), None);
        run.start(100, false);
        assert_eq!(run.update(99), None);
    }

    #[test]
    fn all_pwm_outputs_stay_inside_voltage_limit_in_both_directions() {
        let bound = PWM_PERIOD as f32 * TEST_VOLTS / SUPPLY_VOLTS + 0.5;
        for reverse in [false, true] {
            for now in (0..TEST_US).step_by(250) {
                let values = duties(now, reverse).unwrap();
                for value in values {
                    assert!(value < PWM_PERIOD);
                    assert!((value as f32 - 1024.0).abs() <= bound);
                }
            }
        }
        assert_eq!(duties(TEST_US, false), None);
        assert_eq!(duties(u64::MAX, true), None);
    }

    #[test]
    fn overmodulation_clips_to_valid_compare_values() {
        // At the end of alignment the request is [0, -10.39, +10.39]V
        // relative to mid-bus. The 12V bridge cannot reproduce both peaks.
        assert_eq!(duties(ALIGN_US, false), Some([1024, 0, 2047]));
        let mut reaches_low = false;
        let mut reaches_high = false;
        for now in (0..TEST_US).step_by(250) {
            for value in duties(now, false).unwrap() {
                reaches_low |= value == 0;
                reaches_high |= value == PWM_PERIOD - 1;
            }
        }
        assert!(reaches_low && reaches_high);
    }

    #[test]
    fn reversal_swaps_phase_order() {
        for now in (RAMP_US..TEST_US - RAMP_US).step_by(1_000) {
            let forward = duties(now, false).unwrap();
            let reverse = duties(now, true).unwrap();
            for (a, b) in forward
                .into_iter()
                .zip([reverse[0], reverse[2], reverse[1]])
            {
                assert!(a.abs_diff(PWM_PERIOD - b) <= 1);
            }
        }
    }

    #[test]
    fn startup_aligns_before_rotation_and_keeps_the_phase_continuous() {
        for now in (0..=ALIGN_US).step_by(1_000) {
            let forward = duties(now, false).unwrap();
            assert_eq!(forward[0], PWM_PERIOD / 2);
            assert_eq!(forward, duties(now, true).unwrap());
            assert!(forward[1] <= PWM_PERIOD / 2);
            assert!(forward[2] >= PWM_PERIOD / 2);
        }
        let before = duties(ALIGN_US - 1, false).unwrap();
        let after = duties(ALIGN_US + 1, false).unwrap();
        assert!(
            before
                .into_iter()
                .zip(after)
                .all(|(a, b)| a.abs_diff(b) <= 1)
        );
        assert!(duties(ALIGN_US + 100_000, false).unwrap()[0] > PWM_PERIOD / 2);
        assert!(duties(ALIGN_US + 100_000, true).unwrap()[0] < PWM_PERIOD / 2);
    }

    #[test]
    fn continuous_runs_beyond_test_deadline_but_stops_on_missing_host() {
        let mut run = Run::default();
        assert!(run.start_continuous(0));
        for now in (0..3_000_000).step_by(10_000) {
            if now % 100_000 == 0 {
                assert!(run.keep_alive(now));
            }
            assert!(run.update(now).is_some());
            assert!(!run.start(now, false));
        }
        for now in (3_000_000..3_400_000).step_by(10_000) {
            assert!(run.update(now).is_some());
        }
        assert_eq!(run.update(3_400_000), None);
        assert!(!run.keep_alive(3_400_001));
        assert!(!run.active());
    }

    #[test]
    fn late_heartbeat_cannot_revive_motor_or_extend_a_pulse() {
        let mut run = Run::default();
        run.start_continuous(0);
        assert!(!run.keep_alive(KEEPALIVE_US));
        assert_eq!(run.update(KEEPALIVE_US), None);
        run.start(KEEPALIVE_US, false);
        assert!(!run.keep_alive(KEEPALIVE_US + 1));
        assert!(!run.is_continuous());
    }

    #[test]
    fn continuous_waveform_stays_balanced_and_does_not_lose_phase_over_time() {
        for now in (ALIGN_US..ALIGN_US + ELECTRICAL_PERIOD_US).step_by(250) {
            let values = continuous_duties(now, false);
            assert!(values.iter().all(|&v| v > 0 && v < PWM_PERIOD - 1));
            assert!(values.iter().sum::<u32>().abs_diff(3 * 1024) <= 1);
            assert_eq!(values, continuous_duties(now + 86_400_000_000, false));
        }
    }

    #[test]
    fn runtime_configuration_rejects_invalid_values_without_mutating() {
        let mut run = Run::default();
        for parameters in [
            Parameters {
                voltage: f32::NAN,
                frequency_hz: 8.0,
            },
            Parameters {
                voltage: 2.01,
                frequency_hz: 8.0,
            },
            Parameters {
                voltage: 1.0,
                frequency_hz: f32::INFINITY,
            },
            Parameters {
                voltage: 1.0,
                frequency_hz: -0.01,
            },
        ] {
            assert!(!run.configure(parameters));
            assert_eq!(run.parameters(), Parameters::default());
        }
    }

    #[test]
    fn live_frequency_change_keeps_phase_and_voltage_change_slews() {
        let mut run = Run::default();
        run.start_continuous(0);
        for now in (0..=300_000).step_by(500) {
            assert!(run.keep_alive(now));
            assert!(run.update(now).is_some());
        }
        let angle = run.angle;
        let volts = run.applied_volts;
        assert!(run.configure(Parameters {
            voltage: 0.5,
            frequency_hz: 12.0
        }));
        assert_eq!(run.angle, angle);
        assert_eq!(run.applied_volts, volts);
        assert!(run.update(300_500).is_some());
        assert!((run.angle - angle).abs() < 0.04);
        assert!((run.applied_volts - volts).abs() <= 0.0026);
    }

    #[test]
    fn requested_parameter_boundaries_are_inclusive() {
        assert!(
            Parameters {
                voltage: 0.5,
                frequency_hz: 0.0
            }
            .valid()
        );
        assert!(
            Parameters {
                voltage: 2.0,
                frequency_hz: 100.0
            }
            .valid()
        );
        assert!(
            !Parameters {
                voltage: 0.49,
                frequency_hz: 8.0
            }
            .valid()
        );
        assert!(
            !Parameters {
                voltage: 1.0,
                frequency_hz: 100.01
            }
            .valid()
        );
    }

    #[test]
    fn changing_direction_stops_and_late_heartbeat_cannot_restart() {
        let mut run = Run::default();
        run.start_continuous(0);
        assert!(!run.configure_direction(false));
        assert!(run.active());
        assert!(run.configure_direction(true));
        assert!(!run.active());
        assert!(!run.keep_alive(1));
        assert_eq!(run.update(1), None);
        assert!(run.start_continuous(2));
        assert!(run.reverse);
    }

    #[test]
    fn zero_frequency_holds_a_field_and_still_requires_a_lease() {
        let mut run = Run::default();
        assert!(run.configure(Parameters {
            voltage: 0.5,
            frequency_hz: 0.0
        }));
        assert!(!run.active());
        run.start_continuous(0);
        let mut held = None;
        for now in (0..=800_000).step_by(500) {
            if now % 100_000 == 0 {
                assert!(run.keep_alive(now));
            }
            let pwm = run.update(now).unwrap();
            if now == 300_000 {
                held = Some(pwm);
            }
            if now >= 300_000 {
                assert_eq!(Some(pwm), held);
            }
        }
        let pwm = held.unwrap();
        assert_ne!(pwm, [PWM_PERIOD / 2; 3]);
        for now in (800_500..1_300_000).step_by(500) {
            assert!(run.update(now).is_some());
        }
        assert_eq!(run.update(1_300_000), None);
        assert!(!run.active());
    }

    #[test]
    fn live_change_to_zero_decelerates_then_keeps_phase_stationary() {
        let mut run = Run::default();
        run.start_continuous(0);
        for now in (0..=500_000).step_by(500) {
            if now % 100_000 == 0 {
                run.keep_alive(now);
            }
            run.update(now).unwrap();
        }
        assert!(run.applied_hz > 0.0);
        let phase = run.angle;
        assert!(run.configure(Parameters {
            voltage: 1.0,
            frequency_hz: 0.0
        }));
        assert_eq!(run.angle, phase);
        let mut held = None;
        for now in (500_500..=1_000_000).step_by(500) {
            if now % 100_000 == 0 {
                run.keep_alive(now);
            }
            let pwm = run.update(now).unwrap();
            if now == 800_000 {
                held = Some(pwm);
            }
            if now >= 800_000 {
                assert_eq!(Some(pwm), held);
            }
        }
        assert_eq!(run.applied_hz, 0.0);
        run.stop();
        assert_eq!(run.update(1_000_500), None);
    }

    #[test]
    fn reverse_mode_generates_the_opposite_rotating_field() {
        let mut forward = Run::default();
        let mut reverse = Run::default();
        reverse.configure_direction(true);
        forward.start_continuous(0);
        reverse.start_continuous(0);
        for now in (0..1_000_000).step_by(500) {
            forward.keep_alive(now);
            reverse.keep_alive(now);
            let a = forward.update(now).unwrap();
            let b = reverse.update(now).unwrap();
            assert!((forward.angle + reverse.angle).abs() < 0.00001);
            for (a, b) in a.into_iter().zip([b[0], b[2], b[1]]) {
                assert!(a.abs_diff(PWM_PERIOD - b) <= 1);
            }
        }
    }

    #[test]
    fn high_frequency_start_ramps_up_and_keeps_legal_pwm() {
        let mut run = Run::default();
        run.configure(Parameters {
            voltage: 2.0,
            frequency_hz: 100.0,
        });
        run.start_continuous(0);
        let mut previous_hz = 0.0;
        for now in (0..=2_500_000).step_by(500) {
            run.keep_alive(now);
            let duties = run.update(now).unwrap();
            assert!(duties.iter().all(|&v| v < PWM_PERIOD));
            assert!(run.applied_hz - previous_hz < 0.026);
            previous_hz = run.applied_hz;
        }
        assert_eq!(run.applied_hz, 100.0);
    }
}
