//! A small, clock-free time vocabulary.
//!
//! Why not just use [`std::time`]? Because the higher-level crates emit a stream
//! of [`crate::TransitEvent`]s and the diagnostics crate computes throughput and
//! latency from the *timestamps on those events*. If the library reached for the
//! wall clock internally, identical inputs would produce different reports and
//! tests could not assert on numbers. So instead, time is a plain value that the
//! caller supplies. The mock backend uses a deterministic monotonic counter;
//! real backends stamp [`Timestamp::now`] at the I/O boundary.

use core::fmt;
use core::ops::{Add, Sub};

/// A monotonic duration with nanosecond resolution.
///
/// This mirrors the subset of [`std::time::Duration`] the toolkit needs, but is
/// `Copy`, cheap, and trivially serializable. Convert with [`Duration::from`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Duration {
    nanos: u128,
}

impl Duration {
    /// A zero-length duration.
    pub const ZERO: Duration = Duration { nanos: 0 };

    /// Construct from a whole number of nanoseconds.
    pub const fn from_nanos(nanos: u128) -> Self {
        Duration { nanos }
    }

    /// Construct from a whole number of milliseconds.
    pub const fn from_millis(millis: u64) -> Self {
        Duration {
            nanos: millis as u128 * 1_000_000,
        }
    }

    /// Construct from a whole number of seconds.
    pub const fn from_secs(secs: u64) -> Self {
        Duration {
            nanos: secs as u128 * 1_000_000_000,
        }
    }

    /// Total nanoseconds.
    pub const fn as_nanos(self) -> u128 {
        self.nanos
    }

    /// Total whole milliseconds (truncating).
    pub const fn as_millis(self) -> u128 {
        self.nanos / 1_000_000
    }

    /// Fractional seconds as `f64`. Handy for throughput math.
    pub fn as_secs_f64(self) -> f64 {
        self.nanos as f64 / 1_000_000_000.0
    }

    /// `true` if this is exactly zero.
    pub const fn is_zero(self) -> bool {
        self.nanos == 0
    }
}

impl From<std::time::Duration> for Duration {
    fn from(d: std::time::Duration) -> Self {
        Duration {
            nanos: d.as_nanos(),
        }
    }
}

impl fmt::Debug for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Render in the most readable unit for the magnitude.
        if self.nanos >= 1_000_000_000 {
            write!(f, "{:.3}s", self.as_secs_f64())
        } else if self.nanos >= 1_000_000 {
            write!(f, "{}ms", self.as_millis())
        } else if self.nanos >= 1_000 {
            write!(f, "{}µs", self.nanos / 1_000)
        } else {
            write!(f, "{}ns", self.nanos)
        }
    }
}

/// An absolute point in time, measured in nanoseconds since an unspecified but
/// fixed epoch (Unix epoch when produced by [`Timestamp::now`], or zero-based
/// when produced by a deterministic test clock).
///
/// Only *differences* between timestamps are meaningful across producers; do not
/// compare a deterministic test timestamp against a wall-clock one.
///
/// ```
/// use pax_core::Timestamp;
/// let t0 = Timestamp::from_millis(1_000);
/// let t1 = Timestamp::from_millis(1_250);
/// assert_eq!((t1 - t0).as_millis(), 250);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Timestamp {
    nanos: u128,
}

impl Timestamp {
    /// The zero instant. Useful as the base for a deterministic test clock.
    pub const ZERO: Timestamp = Timestamp { nanos: 0 };

    /// Construct from nanoseconds since the epoch.
    pub const fn from_nanos(nanos: u128) -> Self {
        Timestamp { nanos }
    }

    /// Construct from milliseconds since the epoch.
    pub const fn from_millis(millis: u64) -> Self {
        Timestamp {
            nanos: millis as u128 * 1_000_000,
        }
    }

    /// Nanoseconds since the epoch.
    pub const fn as_nanos(self) -> u128 {
        self.nanos
    }

    /// Read the system wall clock as a [`Timestamp`].
    ///
    /// This is the *only* function in the entire workspace that consults the
    /// real clock, and library internals never call it — it exists for real
    /// backends to stamp events at the I/O boundary, and for application code.
    /// Tests use [`Timestamp::ZERO`] plus a counter instead.
    pub fn now() -> Self {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Timestamp {
            nanos: since.as_nanos(),
        }
    }

    /// Saturating difference: `self - earlier`, clamped at zero if `self` is the
    /// earlier of the two (so it never underflows).
    pub fn saturating_since(self, earlier: Timestamp) -> Duration {
        Duration {
            nanos: self.nanos.saturating_sub(earlier.nanos),
        }
    }
}

impl Sub for Timestamp {
    type Output = Duration;
    /// Panics on underflow in debug builds; prefer [`Timestamp::saturating_since`]
    /// when ordering is not guaranteed.
    fn sub(self, rhs: Timestamp) -> Duration {
        Duration {
            nanos: self.nanos - rhs.nanos,
        }
    }
}

impl Add<Duration> for Timestamp {
    type Output = Timestamp;
    fn add(self, rhs: Duration) -> Timestamp {
        Timestamp {
            nanos: self.nanos + rhs.nanos,
        }
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Timestamp({}ms)", self.nanos / 1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_unit_conversions() {
        assert_eq!(Duration::from_secs(2).as_millis(), 2_000);
        assert_eq!(Duration::from_millis(1_500).as_secs_f64(), 1.5);
        assert!(Duration::ZERO.is_zero());
    }

    #[test]
    fn timestamp_difference_is_a_duration() {
        let a = Timestamp::from_millis(10);
        let b = Timestamp::from_millis(35);
        assert_eq!((b - a).as_millis(), 25);
        assert_eq!(a.saturating_since(b), Duration::ZERO);
    }

    #[test]
    fn duration_debug_picks_readable_unit() {
        assert_eq!(format!("{:?}", Duration::from_secs(3)), "3.000s");
        assert_eq!(format!("{:?}", Duration::from_millis(250)), "250ms");
        assert_eq!(format!("{:?}", Duration::from_nanos(4_000)), "4µs");
    }
}
