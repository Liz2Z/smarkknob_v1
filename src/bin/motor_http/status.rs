//! Immutable snapshot captured by control, formatted only by the network task.
use core::{fmt::Write, sync::atomic::Ordering::Relaxed};
use motor_control::{Parameters, decimal::Fixed};

#[derive(Clone, Copy)]
pub struct KnobStatus {
    pub mode: &'static str,
    pub active: bool,
    pub calibrated: bool,
    pub phase: &'static str,
    pub position: i32,
    pub angle_deg: f32,
    pub strength: f32,
    pub step_deg: f32,
    pub voltage: f32,
    pub error: &'static str,
    pub sensor_direction: i32,
}

#[derive(Clone, Copy)]
pub struct Status {
    pub running: bool,
    pub fault: bool,
    pub mode: &'static str,
    pub reverse: bool,
    pub parameters: Parameters,
    pub captured_us: u64,
    pub control_max_gap_us: u64,
    pub encoder: crate::as5600::Telemetry,
    pub knob: KnobStatus,
}

impl Status {
    pub fn json(&self) -> heapless::String<1024> {
        let started = crate::now_us();
        let mut body = heapless::String::new();
        let _ = write!(
            body,
            "{{\"running\":{},\"fault\":{},\"mode\":\"{}\",\"direction\":\"{}\",\"voltage\":{},\"frequency_hz\":{},\"lease_ms\":0,\"uptime_ms\":{},\"control_max_gap_us\":{},\"status_format_us\":{},\"network_poll_us\":{},\"control_work_us\":{}",
            self.running,
            self.fault,
            self.mode,
            if self.reverse { "reverse" } else { "forward" },
            Fixed::<3>(self.parameters.voltage),
            Fixed::<3>(self.parameters.frequency_hz),
            self.captured_us / 1000,
            self.control_max_gap_us,
            crate::STATUS_FORMAT_MAX_US.load(Relaxed),
            crate::NETWORK_POLL_MAX_US.load(Relaxed),
            crate::CONTROL_WORK_MAX_US.load(Relaxed)
        );
        self.encoder.append_json(&mut body, self.captured_us);
        let h = &self.knob;
        let _ = write!(
            body,
            ",\"haptic\":{{\"mode\":\"{}\",\"active\":{},\"calibrated\":{},\"phase\":\"{}\",\"position\":{},\"angle_deg\":{},\"strength\":{},\"step_deg\":{},\"voltage\":{},\"error\":\"{}\",\"sensor_direction\":{}}}}}",
            h.mode,
            h.active,
            h.calibrated,
            h.phase,
            h.position,
            Fixed::<2>(h.angle_deg),
            Fixed::<2>(h.strength),
            Fixed::<1>(h.step_deg),
            Fixed::<3>(h.voltage),
            h.error,
            h.sensor_direction
        );
        crate::record_max(
            &crate::STATUS_FORMAT_MAX_US,
            crate::now_us().saturating_sub(started),
        );
        body
    }
}
