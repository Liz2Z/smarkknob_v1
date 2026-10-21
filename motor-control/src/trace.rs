//! Fixed-storage feedback history. Encoding is explicit little endian, not ABI.
#[derive(Clone, Copy)]
pub struct Point {
    pub at_us: u32,
    pub sample_age_us: u16,
    pub raw: u16,
    pub velocity_centi: i16,
    pub voltage_milli: i16,
    pub error_centi_deg: i16,
    pub flags: u16,
    /// Wall time from control entry through applying PWM, including preemption.
    pub calculation_us: u16,
}
impl Point {
    const ZERO: Self = Self {
        at_us: 0,
        sample_age_us: 0,
        raw: 0,
        velocity_centi: 0,
        voltage_milli: 0,
        error_centi_deg: 0,
        flags: 0,
        calculation_us: 0,
    };
}
pub struct Ring<const N: usize> {
    points: [Point; N],
    next: usize,
    count: usize,
}
impl<const N: usize> Ring<N> {
    pub const fn new() -> Self {
        assert!(N > 0);
        Self {
            points: [Point::ZERO; N],
            next: 0,
            count: 0,
        }
    }
    pub fn reset(&mut self) {
        self.next = 0;
        self.count = 0;
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn push(&mut self, point: Point) {
        self.points[self.next] = point;
        self.next = (self.next + 1) % N;
        self.count = (self.count + 1).min(N);
    }
    /// A smaller output still contains the newest complete points in order.
    pub fn encode(&self, out: &mut [u8]) -> usize {
        if out.len() < 8 {
            return 0;
        }
        let count = self.count.min((out.len() - 8) / 20).min(u16::MAX as usize);
        out[..4].copy_from_slice(b"HKTR");
        out[4..6].copy_from_slice(&2u16.to_le_bytes());
        out[6..8].copy_from_slice(&(count as u16).to_le_bytes());
        let start = (self.next + N - count) % N;
        for i in 0..count {
            let p = self.points[(start + i) % N];
            let b = &mut out[8 + 20 * i..8 + 20 * (i + 1)];
            b[0..4].copy_from_slice(&p.at_us.to_le_bytes());
            b[4..6].copy_from_slice(&p.sample_age_us.to_le_bytes());
            b[6..8].copy_from_slice(&p.raw.to_le_bytes());
            b[8..10].copy_from_slice(&p.velocity_centi.to_le_bytes());
            b[10..12].copy_from_slice(&p.voltage_milli.to_le_bytes());
            b[12..14].copy_from_slice(&p.error_centi_deg.to_le_bytes());
            b[14..16].copy_from_slice(&p.flags.to_le_bytes());
            b[16..18].copy_from_slice(&p.calculation_us.to_le_bytes());
            b[18..20].fill(0);
        }
        8 + 20 * count
    }
}
impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trace_wrap_and_small_exports_preserve_latest_order_and_signed_values() {
        let mut ring = Ring::<3>::new();
        for i in 0..5 {
            ring.push(Point {
                at_us: i,
                raw: 4095,
                velocity_centi: -123,
                voltage_milli: -1000,
                calculation_us: 1234,
                ..Point::ZERO
            });
        }
        let mut out = [0; 68];
        assert_eq!(ring.encode(&mut out), 68);
        assert_eq!(&out[..8], b"HKTR\x02\x00\x03\x00");
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), 2);
        assert_eq!(i16::from_le_bytes(out[18..20].try_into().unwrap()), -1000);
        assert_eq!(u16::from_le_bytes(out[24..26].try_into().unwrap()), 1234);
        assert_eq!(ring.encode(&mut out[..28]), 28);
        assert_eq!(u32::from_le_bytes(out[8..12].try_into().unwrap()), 4);
        ring.reset();
        assert_eq!(ring.len(), 0);
        assert_eq!(ring.encode(&mut out), 8);
        assert_eq!(ring.encode(&mut out[..7]), 0);
    }
}
