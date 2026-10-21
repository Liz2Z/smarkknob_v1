use alloc::vec::Vec;
use core::cell::RefCell;
use motor_control::{
    encoder::Snapshot,
    haptic::Haptics,
    trace::{Point, Ring},
};
static HISTORY: critical_section::Mutex<RefCell<Ring<256>>> =
    critical_section::Mutex::new(RefCell::new(Ring::new()));
pub fn reset() {
    critical_section::with(|cs| HISTORY.borrow(cs).borrow_mut().reset());
}
pub fn record(now: u64, sample: Snapshot, h: &Haptics, calculation_us: u64) {
    let point = Point {
        at_us: now as u32,
        sample_age_us: sample.sampled_at.map_or(u16::MAX, |at| {
            now.saturating_sub(at).min(u16::MAX as u64) as u16
        }),
        raw: sample.reading.map_or(u16::MAX, |r| r.raw_angle),
        velocity_centi: (h.velocity_rad_s() * 100.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16,
        voltage_milli: (h.commanded_volts * 1000.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16,
        error_centi_deg: (h.detent_error_deg() * 100.0).clamp(i16::MIN as f32, i16::MAX as f32)
            as i16,
        calculation_us: calculation_us.min(u16::MAX as u64) as u16,
        flags: u16::from(h.active())
            | (u16::from(h.phase() == "calibrating") << 1)
            | (u16::from(h.coasting) << 2),
    };
    critical_section::with(|cs| HISTORY.borrow(cs).borrow_mut().push(point));
}
pub fn snapshot() -> Option<Vec<u8>> {
    let count = critical_section::with(|cs| HISTORY.borrow(cs).borrow().len());
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(8 + 20 * count).ok()?;
    bytes.resize(8 + 20 * count, 0);
    let len = critical_section::with(|cs| HISTORY.borrow(cs).borrow().encode(&mut bytes));
    bytes.truncate(len);
    Some(bytes)
}
