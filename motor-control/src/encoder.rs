//! AS5600 decoding and volatile feedback configuration. Never writes OTP.

/// CONF: SF=11 (2x, 0.286ms), nominal power, watchdog off while controlling.
/// Preserve factory/reserved bits, output format, hysteresis and FTH. The worker
/// restores the original complete CONF when idle; no BURN command is used.
pub const fn feedback_configuration(original: u16) -> u16 {
    (original & !0x2303) | 0x0300
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub status: u8,
    pub raw_angle: u16,
}

impl Reading {
    pub fn decode(status: u8, angle: [u8; 2]) -> Self {
        Self {
            status,
            raw_angle: u16::from_be_bytes(angle) & 0x0fff,
        }
    }

    pub fn degrees(self) -> f32 {
        self.raw_angle as f32 * (360.0 / 4096.0)
    }

    pub fn magnet(self) -> &'static str {
        if self.status & 0x20 == 0 {
            "missing"
        } else {
            match self.status & 0x18 {
                0 => "ok",
                0x10 => "weak",
                0x08 => "strong",
                _ => "invalid",
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Snapshot {
    pub reading: Option<Reading>,
    pub sampled_at: Option<u64>,
    pub error: Option<&'static str>,
}

impl Snapshot {
    /// A consumer must explicitly choose its maximum age; connected does not
    /// mean fresh, and raw data under a magnetic warning remains unverified.
    pub fn valid_angle(&self, now: u64, max_age_us: u64) -> Option<f32> {
        let reading = self.reading?;
        let age = now.checked_sub(self.sampled_at?)?;
        (reading.magnet() == "ok" && age < max_age_us).then(|| reading.degrees())
    }

    pub fn record(&mut self, now: u64, result: Result<Reading, &'static str>) {
        match result {
            Ok(reading) => {
                self.reading = Some(reading);
                self.sampled_at = Some(now);
                self.error = None;
            }
            Err(error) => {
                // Never leave a failed/disconnected sample looking live.
                self.reading = None;
                self.sampled_at = None;
                self.error = Some(error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_configuration_preserves_all_unrelated_bits() {
        for original in 0..=u16::MAX {
            let c = feedback_configuration(original);
            assert_eq!(c & 0x2303, 0x0300);
            assert_eq!(c & !0x2303, original & !0x2303);
        }
    }

    #[test]
    fn angle_is_big_endian_12_bit_and_wraps_below_360() {
        assert_eq!(Reading::decode(0x20, [0xf8, 0x00]).raw_angle, 2048);
        assert_eq!(Reading::decode(0x20, [0x08, 0x00]).degrees(), 180.0);
        assert_eq!(Reading::decode(0x20, [0, 0]).degrees(), 0.0);
        let max = Reading::decode(0x20, [0x0f, 0xff]).degrees();
        assert!(max > 359.9 && max < 360.0);
    }

    #[test]
    fn missing_weak_strong_and_contradictory_fields_are_not_ok() {
        for (status, expected) in [
            (0, "missing"),
            (0x10, "missing"),
            (0x20, "ok"),
            (0x30, "weak"),
            (0x28, "strong"),
            (0x38, "invalid"),
        ] {
            assert_eq!(Reading::decode(status, [0, 0]).magnet(), expected);
        }
    }

    #[test]
    fn disconnect_clears_stale_data_and_reconnect_restores_it() {
        let mut state = Snapshot::default();
        state.record(100, Ok(Reading::decode(0x20, [0x08, 0])));
        assert_eq!(state.sampled_at, Some(100));
        state.record(200, Err("nack"));
        assert_eq!(state.reading, None);
        assert_eq!(state.sampled_at, None);
        assert_eq!(state.error, Some("nack"));
        state.record(300, Ok(Reading::decode(0x20, [0x04, 0])));
        assert_eq!(state.reading.unwrap().degrees(), 90.0);
        assert_eq!(state.error, None);
    }

    #[test]
    fn valid_feedback_requires_a_fresh_normal_field_and_monotonic_time() {
        let mut state = Snapshot::default();
        state.record(100_000, Ok(Reading::decode(0x20, [8, 0])));
        assert_eq!(state.valid_angle(124_999, 25_000), Some(180.0));
        assert_eq!(state.valid_angle(125_000, 25_000), None);
        assert_eq!(state.valid_angle(99_999, 25_000), None);
        state.record(125_000, Ok(Reading::decode(0x30, [8, 0])));
        assert_eq!(state.valid_angle(125_001, 25_000), None);
        state.record(125_002, Err("nack"));
        assert_eq!(state.valid_angle(125_003, 25_000), None);
    }
}
