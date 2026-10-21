//! Bounded fixed precision JSON numbers without the general float formatter.
//! The C3 has no FPU; float-to-decimal conversion in the status path must not
//! hold up commutation. Only 0–3 decimal places are needed by this protocol.
use core::fmt;

pub struct Fixed<const PLACES: usize>(pub f32);

impl<const PLACES: usize> fmt::Display for Fixed<PLACES> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        if PLACES > 3 {
            return Err(fmt::Error);
        }
        let value = self.0 as f64;
        let scale = [1u64, 10, 100, 1000][PLACES];
        let scaled = value.abs() * scale as f64;
        if !scaled.is_finite() || scaled >= u64::MAX as f64 {
            return out.write_str("null");
        }
        let whole = scaled as u64;
        let fraction = scaled - whole as f64;
        // Round ties to even, matching Rust's fixed precision formatting.
        let rounded = whole + u64::from(fraction > 0.5 || (fraction == 0.5 && whole & 1 == 1));
        if value.is_sign_negative() {
            out.write_str("-")?;
        }
        if PLACES == 0 {
            write!(out, "{rounded}")
        } else {
            write!(
                out,
                "{}.{:0width$}",
                rounded / scale,
                rounded % scale,
                width = PLACES
            )
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn status_numbers_match_standard_format_for_signed_values_and_large_angles() {
        for i in -10_000..=10_000 {
            let x = i as f32 * 0.12345;
            assert_eq!(std::format!("{}", Fixed::<1>(x)), std::format!("{x:.1}"));
            assert_eq!(std::format!("{}", Fixed::<2>(x)), std::format!("{x:.2}"));
            assert_eq!(std::format!("{}", Fixed::<3>(x)), std::format!("{x:.3}"));
        }
        for x in [0.0, -0.0, 2.5, 3.5, -2.5, 12_345_678.0, -90_000_000.0] {
            assert_eq!(std::format!("{}", Fixed::<0>(x)), std::format!("{x:.0}"));
            assert_eq!(std::format!("{}", Fixed::<3>(x)), std::format!("{x:.3}"));
        }
    }

    #[test]
    fn non_finite_values_never_create_invalid_json_numbers() {
        for x in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::MAX] {
            assert_eq!(std::format!("{}", Fixed::<3>(x)), "null");
        }
    }
}
