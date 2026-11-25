//! PMU counter multiplexing detection and scaling (#219).
//!
//! When more hardware events are requested than the PMU has physical counters,
//! the kernel time-shares them and a raw count only covers the time the event
//! was on the PMU. Opening events with [`READ_FORMAT`] makes `read(2)` return
//! `time_enabled` and `time_running`, which this module uses to scale the count
//! like `perf stat` does. Everything here is pure and platform independent;
//! the `perf_event_open` reader (#49) feeds it [`RawReading`]s.

use serde::Serialize;

/// `PERF_FORMAT_TOTAL_TIME_ENABLED`
pub const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
/// `PERF_FORMAT_TOTAL_TIME_RUNNING`
pub const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
/// `read_format` to set in `perf_event_attr` for non-group counters.
pub const READ_FORMAT: u64 = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;

/// Layout returned by `read(2)` on a counter opened with [`READ_FORMAT`].
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawReading {
    pub value: u64,
    pub time_enabled: u64,
    pub time_running: u64,
}

impl RawReading {
    pub const SIZE: usize = 24;

    /// Decodes a native-endian `read(2)` buffer. `None` if it is too short.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        let word = |i: usize| u64::from_ne_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
        Some(Self {
            value: word(0),
            time_enabled: word(1),
            time_running: word(2),
        })
    }
}

/// A counter value after multiplexing correction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaledCounter {
    /// Count to report: scaled when multiplexed, otherwise the raw count.
    pub value: u64,
    /// Count as read from the kernel.
    pub raw: u64,
    pub time_enabled: u64,
    pub time_running: u64,
}

impl ScaledCounter {
    /// Scales `value * time_enabled / time_running` when `0 < time_running < time_enabled`.
    ///
    /// If the event never ran (`time_running == 0` while enabled) there is nothing
    /// to scale from: the value stays as read and [`Self::never_ran`] is true.
    /// The multiplication uses `u128` and saturates at `u64::MAX`.
    pub fn from_raw(r: RawReading) -> Self {
        let value = if r.time_running > 0 && r.time_running < r.time_enabled {
            let scaled = r.value as u128 * r.time_enabled as u128 / r.time_running as u128;
            u64::try_from(scaled).unwrap_or(u64::MAX)
        } else {
            r.value
        };
        Self {
            value,
            raw: r.value,
            time_enabled: r.time_enabled,
            time_running: r.time_running,
        }
    }

    /// True if the count was extrapolated.
    pub fn scaled(&self) -> bool {
        self.time_running > 0 && self.time_running < self.time_enabled
    }

    /// True if the event was enabled but never scheduled on the PMU.
    pub fn never_ran(&self) -> bool {
        self.time_enabled > 0 && self.time_running == 0
    }

    /// Fraction of the enabled time the event was counting, in `0.0..=1.0`.
    /// 1.0 when the event was never enabled.
    pub fn coverage(&self) -> f64 {
        if self.time_enabled == 0 {
            1.0
        } else {
            (self.time_running as f64 / self.time_enabled as f64).min(1.0)
        }
    }

    /// Terminal annotation, `None` when the count is exact.
    pub fn annotation(&self) -> Option<String> {
        if self.never_ran() {
            Some("[not counted: event never scheduled on the PMU]".to_string())
        } else if self.scaled() {
            Some(format!(
                "[scaled: counted {:.1}% of the time]",
                self.coverage() * 100.0
            ))
        } else {
            None
        }
    }

    /// Additive JSON fields for an exported counter.
    pub fn export(&self) -> CounterScaling {
        CounterScaling {
            time_enabled: self.time_enabled,
            time_running: self.time_running,
            scaled: self.scaled(),
            coverage: self.coverage(),
        }
    }
}

/// Fields added to each exported counter object (additive, schema minor bump).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CounterScaling {
    /// Nanoseconds the event was enabled.
    pub time_enabled: u64,
    /// Nanoseconds the event was on the PMU.
    pub time_running: u64,
    /// True if the exported count was extrapolated.
    pub scaled: bool,
    /// `time_running / time_enabled`, `0.0..=1.0`.
    pub coverage: f64,
}

/// Warning text for the terminal when any counter of a benchmark was scaled.
/// `counters` pairs a display name with its readings over all runs.
pub fn multiplex_warning(counters: &[(&str, Vec<ScaledCounter>)]) -> Option<String> {
    let affected: Vec<String> = counters
        .iter()
        .filter_map(|(name, runs)| {
            let worst = runs
                .iter()
                .filter(|c| c.scaled() || c.never_ran())
                .map(|c| c.coverage())
                .fold(None, |m: Option<f64>, c| Some(m.map_or(c, |m| m.min(c))))?;
            Some(format!("{name} ({:.1}%)", worst * 100.0))
        })
        .collect();
    if affected.is_empty() {
        return None;
    }
    Some(format!(
        "Warning: PMU counters were multiplexed, so values are scaled or missing (lowest coverage): {}. \
         Request fewer counters for exact values.",
        affected.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(value: u64, enabled: u64, running: u64) -> RawReading {
        RawReading {
            value,
            time_enabled: enabled,
            time_running: running,
        }
    }

    #[test]
    fn exact_when_always_running() {
        let c = ScaledCounter::from_raw(raw(1000, 500, 500));
        assert_eq!(c.value, 1000);
        assert!(!c.scaled() && !c.never_ran());
        assert_eq!(c.coverage(), 1.0);
        assert_eq!(c.annotation(), None);
    }

    #[test]
    fn scales_by_enabled_over_running() {
        let c = ScaledCounter::from_raw(raw(742, 1000, 742));
        assert_eq!(c.value, 1000);
        let c = ScaledCounter::from_raw(raw(500, 1000, 250));
        assert_eq!((c.value, c.raw), (2000, 500));
        assert!(c.scaled());
        assert_eq!(c.coverage(), 0.25);
        assert_eq!(
            c.annotation().as_deref(),
            Some("[scaled: counted 25.0% of the time]")
        );
    }

    #[test]
    fn never_ran_is_not_scaled() {
        let c = ScaledCounter::from_raw(raw(0, 1000, 0));
        assert_eq!(c.value, 0);
        assert!(!c.scaled() && c.never_ran());
        assert_eq!(c.coverage(), 0.0);
        assert!(c.annotation().unwrap().contains("not counted"));
    }

    #[test]
    fn never_enabled_is_exact() {
        let c = ScaledCounter::from_raw(raw(0, 0, 0));
        assert!(!c.scaled() && !c.never_ran());
        assert_eq!(c.coverage(), 1.0);
    }

    #[test]
    fn running_above_enabled_is_clamped() {
        let c = ScaledCounter::from_raw(raw(10, 100, 101));
        assert_eq!(c.value, 10);
        assert_eq!(c.coverage(), 1.0);
    }

    #[test]
    fn large_values_do_not_overflow() {
        let c = ScaledCounter::from_raw(raw(u64::MAX / 2, 1_000_000_000, 500_000_000));
        assert_eq!(c.value, u64::MAX - 1);
        let c = ScaledCounter::from_raw(raw(u64::MAX, 1000, 1));
        assert_eq!(c.value, u64::MAX);
    }

    #[test]
    fn parses_read_buffer() {
        let mut buf = Vec::new();
        for w in [7u64, 20, 10] {
            buf.extend_from_slice(&w.to_ne_bytes());
        }
        assert_eq!(RawReading::from_bytes(&buf), Some(raw(7, 20, 10)));
        assert_eq!(RawReading::from_bytes(&buf[..23]), None);
        assert_eq!(READ_FORMAT, 3);
    }

    #[test]
    fn export_fields() {
        let json = serde_json::to_value(ScaledCounter::from_raw(raw(1, 4, 1)).export()).unwrap();
        assert_eq!(json["scaled"], true);
        assert_eq!(json["coverage"], 0.25);
        assert_eq!(json["time_enabled"], 4);
        assert_eq!(json["time_running"], 1);
    }

    #[test]
    fn warning_lists_lowest_coverage() {
        let a = vec![
            ScaledCounter::from_raw(raw(1, 100, 100)),
            ScaledCounter::from_raw(raw(1, 100, 80)),
            ScaledCounter::from_raw(raw(1, 100, 50)),
        ];
        let b = vec![ScaledCounter::from_raw(raw(1, 100, 100))];
        let w = multiplex_warning(&[("instructions", a), ("cycles", b.clone())]).unwrap();
        assert!(w.contains("instructions (50.0%)") && !w.contains("cycles"));
        assert_eq!(multiplex_warning(&[("cycles", b)]), None);
    }
}
