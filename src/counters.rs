//! Derived hardware-counter metrics (#213): IPC, CPI, cache miss rate and
//! branch misprediction rate.
//!
//! This module only does the arithmetic. It is independent of how the raw
//! counts are collected (`perf_event_open`, #49), so it is testable on every
//! platform. A rate is `None` when an input counter is missing or a
//! denominator is zero; it is never `NaN`, infinite or `0` in that case.

use serde::{Deserialize, Serialize};

/// Raw counter totals of one benchmark run. `None` means the counter was not
/// collected (not requested, or no PMU support).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CounterSample {
    pub instructions: Option<u64>,
    pub cycles: Option<u64>,
    pub cache_references: Option<u64>,
    pub cache_misses: Option<u64>,
    pub branches: Option<u64>,
    pub branch_misses: Option<u64>,
}

/// Derived efficiency metrics. Rates are fractions (`0.182` = 18.2 %).
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Efficiency {
    /// instructions / cycles
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipc: Option<f64>,
    /// cycles / instructions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpi: Option<f64>,
    /// cache_misses / cache_references
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_miss_rate: Option<f64>,
    /// branch_misses / branches
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_miss_rate: Option<f64>,
}

/// `num / den`, or `None` if either is missing or the denominator is zero.
fn ratio(num: Option<u64>, den: Option<u64>) -> Option<f64> {
    match (num, den) {
        (Some(n), Some(d)) if d > 0 => Some(n as f64 / d as f64),
        _ => None,
    }
}

impl Efficiency {
    /// Derives the metrics of a single run.
    pub fn from_sample(s: &CounterSample) -> Self {
        Self {
            ipc: ratio(s.instructions, s.cycles),
            cpi: ratio(s.cycles, s.instructions),
            cache_miss_rate: ratio(s.cache_misses, s.cache_references),
            branch_miss_rate: ratio(s.branch_misses, s.branches),
        }
    }

    /// Mean of the per-run metrics. Each metric averages only the runs where
    /// it is defined; it is `None` if no run defines it.
    pub fn mean_of(samples: &[CounterSample]) -> Option<Self> {
        let per_run: Vec<Self> = samples.iter().map(Self::from_sample).collect();
        let mean = |f: fn(&Self) -> Option<f64>| {
            let v: Vec<f64> = per_run.iter().filter_map(f).collect();
            (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
        };
        let e = Self {
            ipc: mean(|e| e.ipc),
            cpi: mean(|e| e.cpi),
            cache_miss_rate: mean(|e| e.cache_miss_rate),
            branch_miss_rate: mean(|e| e.branch_miss_rate),
        };
        (!e.is_empty()).then_some(e)
    }

    pub fn is_empty(&self) -> bool {
        self.ipc.is_none()
            && self.cpi.is_none()
            && self.cache_miss_rate.is_none()
            && self.branch_miss_rate.is_none()
    }

    /// Terminal summary, e.g. `IPC 1.15 · CPI 0.87 · cache miss 18.2% · branch miss 3.1%`.
    /// Undefined metrics are left out.
    pub fn summary_line(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(v) = self.ipc {
            parts.push(format!("IPC {v:.2}"));
        }
        if let Some(v) = self.cpi {
            parts.push(format!("CPI {v:.2}"));
        }
        if let Some(v) = self.cache_miss_rate {
            parts.push(format!("cache miss {:.1}%", v * 100.0));
        }
        if let Some(v) = self.branch_miss_rate {
            parts.push(format!("branch miss {:.1}%", v * 100.0));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// Formats a miss-rate change in percentage points,
/// e.g. `18.2% -> 4.5% (-13.7 pp)`.
pub fn format_rate_delta(from: f64, to: f64) -> String {
    format!(
        "{:.1}% -> {:.1}% ({:+.1} pp)",
        from * 100.0,
        to * 100.0,
        (to - from) * 100.0
    )
}

/// Counters that must be sampled together with a requested one so that its
/// rate can be computed. Names follow the `--counters` list of #49.
pub fn required_companions(name: &str) -> &'static [&'static str] {
    match name {
        "cache-misses" => &["cache-references"],
        "cache-references" => &["cache-misses"],
        "branch-misses" => &["branches"],
        "branches" => &["branch-misses"],
        "instructions" => &["cycles"],
        "cycles" => &["instructions"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(i: u64, c: u64, cr: u64, cm: u64, b: u64, bm: u64) -> CounterSample {
        CounterSample {
            instructions: Some(i),
            cycles: Some(c),
            cache_references: Some(cr),
            cache_misses: Some(cm),
            branches: Some(b),
            branch_misses: Some(bm),
        }
    }

    #[test]
    fn derives_all_rates() {
        let e = Efficiency::from_sample(&full(200, 100, 1000, 182, 500, 15));
        assert_eq!(e.ipc, Some(2.0));
        assert_eq!(e.cpi, Some(0.5));
        assert_eq!(e.cache_miss_rate, Some(0.182));
        assert_eq!(e.branch_miss_rate, Some(0.03));
    }

    #[test]
    fn zero_denominator_is_none() {
        let e = Efficiency::from_sample(&full(0, 0, 0, 5, 0, 5));
        assert!(e.is_empty());
    }

    #[test]
    fn zero_numerator_is_zero_rate() {
        let e = Efficiency::from_sample(&full(0, 10, 10, 0, 10, 0));
        assert_eq!(e.ipc, Some(0.0));
        assert_eq!(e.cpi, None);
        assert_eq!(e.cache_miss_rate, Some(0.0));
    }

    #[test]
    fn missing_counters_are_none() {
        let s = CounterSample {
            instructions: Some(10),
            ..Default::default()
        };
        assert!(Efficiency::from_sample(&s).is_empty());
        assert_eq!(Efficiency::mean_of(&[s]), None);
        assert_eq!(Efficiency::mean_of(&[]), None);
    }

    #[test]
    fn mean_is_over_per_run_rates_where_defined() {
        let runs = [
            full(100, 100, 10, 1, 10, 1),
            full(300, 100, 0, 0, 10, 3),
            CounterSample::default(),
        ];
        let e = Efficiency::mean_of(&runs).unwrap();
        assert_eq!(e.ipc, Some(2.0));
        assert_eq!(e.cache_miss_rate, Some(0.1));
        assert!((e.branch_miss_rate.unwrap() - 0.2).abs() < 1e-12);
    }

    #[test]
    fn summary_line_skips_undefined() {
        let e = Efficiency {
            ipc: Some(1.15),
            cpi: Some(0.87),
            cache_miss_rate: Some(0.182),
            branch_miss_rate: None,
        };
        assert_eq!(
            e.summary_line().unwrap(),
            "IPC 1.15 · CPI 0.87 · cache miss 18.2%"
        );
        assert_eq!(Efficiency::default().summary_line(), None);
    }

    #[test]
    fn delta_in_percentage_points() {
        assert_eq!(format_rate_delta(0.182, 0.045), "18.2% -> 4.5% (-13.7 pp)");
    }

    #[test]
    fn json_omits_undefined_and_uses_fractions() {
        let e = Efficiency {
            ipc: Some(1.5),
            cache_miss_rate: Some(0.25),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"ipc":1.5,"cache_miss_rate":0.25}"#
        );
    }

    #[test]
    fn companions_pair_counters() {
        assert_eq!(required_companions("cache-misses"), ["cache-references"]);
        assert_eq!(required_companions("cycles"), ["instructions"]);
        assert!(required_companions("other").is_empty());
    }
}
