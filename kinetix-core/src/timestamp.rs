use serde::{Deserialize, Serialize};

/// A media timestamp expressed as a rational number `value / time_base.1 * time_base.0` seconds.
///
/// `time_base` is `(numerator, denominator)`, e.g. `(1, 90000)` for a 90 kHz clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Timestamp {
    /// Raw tick count in units of `time_base`.
    pub value: i64,
    /// `(numerator, denominator)` of the time base, e.g. `(1, 90000)`.
    pub time_base: (u32, u32),
}

impl Timestamp {
    /// A sentinel value indicating an unknown / unset timestamp.
    pub const NONE: Self = Self {
        value: i64::MIN,
        time_base: (1, 1),
    };

    /// Creates a new `Timestamp`.
    ///
    /// # Examples
    ///
    /// ```
    /// use kinetix_core::Timestamp;
    /// let ts = Timestamp::new(90_000, (1, 90_000));
    /// assert!((ts.as_secs_f64() - 1.0).abs() < 1e-9);
    /// ```
    #[inline]
    pub fn new(value: i64, time_base: (u32, u32)) -> Self {
        Self { value, time_base }
    }

    /// Returns `true` if this is the sentinel "no timestamp" value.
    #[inline]
    pub fn is_none(self) -> bool {
        self.value == i64::MIN
    }

    /// Converts the timestamp to seconds as an `f64`.
    ///
    /// Returns `f64::NAN` if `self.is_none()` or the time base denominator is zero.
    pub fn as_secs_f64(self) -> f64 {
        if self.is_none() || self.time_base.1 == 0 {
            return f64::NAN;
        }
        self.value as f64 * (self.time_base.0 as f64 / self.time_base.1 as f64)
    }

    /// Converts the timestamp to milliseconds as an `i64`, rounding down.
    ///
    /// Returns `None` if `self.is_none()` or the time base denominator is zero.
    pub fn as_millis(self) -> Option<i64> {
        if self.is_none() || self.time_base.1 == 0 {
            return None;
        }
        let ms = self.value as i128 * self.time_base.0 as i128 * 1_000
            / self.time_base.1 as i128;
        Some(ms as i64)
    }

    /// Re-expresses this timestamp in a different time base.
    ///
    /// Returns `None` if either time base denominator is zero or `self.is_none()`.
    pub fn rescale(self, new_base: (u32, u32)) -> Option<Self> {
        if self.is_none() || self.time_base.1 == 0 || new_base.1 == 0 || new_base.0 == 0 {
            return None;
        }
        // value_new = value * (old_num / old_den) / (new_num / new_den)
        //           = value * old_num * new_den / (old_den * new_num)
        let num = self.value as i128
            * self.time_base.0 as i128
            * new_base.1 as i128;
        let den = self.time_base.1 as i128 * new_base.0 as i128;
        Some(Self::new((num / den) as i64, new_base))
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_none() {
            write!(f, "NOPTS")
        } else {
            write!(
                f,
                "{:.3}s ({}/{} @ {}/{})",
                self.as_secs_f64(),
                self.value,
                self.time_base.1,
                self.time_base.0,
                self.time_base.1
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_secs_f64() {
        let ts = Timestamp::new(90_000, (1, 90_000));
        assert!((ts.as_secs_f64() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_rescale() {
        let ts = Timestamp::new(90_000, (1, 90_000));
        let rescaled = ts.rescale((1, 1_000)).unwrap();
        assert_eq!(rescaled.value, 1_000);
    }

    #[test]
    fn test_none_sentinel() {
        assert!(Timestamp::NONE.is_none());
        assert!(Timestamp::NONE.as_secs_f64().is_nan());
    }
}
