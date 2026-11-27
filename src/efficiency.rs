//! Efficiency metrics (#373, `docs/specs/efficiency-metrics.md`): the energy-delay
//! products EDP (`E·t`) and ED²P (`E·t²`), and, with `--work N[unit]`, time, energy and
//! counters per unit of work, throughput and work per joule.
//!
//! The values are computed for every run from the measured series and stored like
//! `--output-metric` series (`custom_metrics`), so `--metric custom:<name>`, `--pareto`
//! and `--budget` use them without further code. The descriptors in `perfratio.metrics[]`
//! mark them `kind: derived`.

use std::collections::BTreeMap;

use crate::benchmark::benchmark_result::{BenchmarkResult, MetricSummary};
use crate::metric::{set_custom_meta_if_absent, CustomMeta};

/// Names of the metrics that need no `--work`
pub const ENERGY_DELAY: [&str; 2] = ["edp", "ed2p"];
/// Names of the metrics that need `--work`
pub const PER_WORK: [&str; 8] = [
    "time_per_op",
    "energy_per_op",
    "ops_per_second",
    "ops_per_joule",
    "instructions_per_op",
    "cycles_per_op",
    "edp_per_op",
    "ed2p_per_op",
];

/// Whether `name` is one of the derived efficiency metrics
pub fn is_derived(name: &str) -> bool {
    ENERGY_DELAY.contains(&name) || PER_WORK.contains(&name)
}

/// The amount of work of one run, `--work N[unit]`
#[derive(Debug, Clone, PartialEq)]
pub struct Work {
    pub amount: f64,
    /// What is counted (`op` unless given)
    pub unit: String,
}

impl std::str::FromStr for Work {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let bytes = s.as_bytes();
        let mut end = 0;
        while end < bytes.len()
            && (bytes[end].is_ascii_digit() || matches!(bytes[end], b'.' | b'_'))
        {
            end += 1;
        }
        // exponent: `1e6`, `2.5E-3`, but not the unit `events`
        if end > 0 && end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
            let mut e = end + 1;
            if e < bytes.len() && matches!(bytes[e], b'+' | b'-') {
                e += 1;
            }
            if e < bytes.len() && bytes[e].is_ascii_digit() {
                while e < bytes.len() && bytes[e].is_ascii_digit() {
                    e += 1;
                }
                end = e;
            }
        }
        let amount: f64 = s[..end].replace('_', "").parse().map_err(|_| {
            format!("invalid work '{s}': expected a number with an optional unit, e.g. 1000000ops")
        })?;
        if !amount.is_finite() || amount <= 0.0 {
            return Err(format!("invalid work '{s}': the amount must be positive"));
        }
        let unit = s[end..].trim();
        if !unit
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '%' | ' '))
        {
            return Err(format!(
                "invalid work unit '{unit}': use letters, digits, '_' and '-'"
            ));
        }
        Ok(Work {
            amount,
            unit: if unit.is_empty() { "op" } else { unit }.to_string(),
        })
    }
}

/// What a derived metric is computed from, for error messages
pub fn requirement(name: &str) -> &'static str {
    match name {
        "edp" | "ed2p" => "it needs an energy reading in every run ('--energy')",
        "time_per_op" | "ops_per_second" => "it needs '--work N[unit]'",
        "instructions_per_op" | "cycles_per_op" => {
            "it needs '--work N[unit]' and the hardware counters ('--counters', Linux)"
        }
        _ => "it needs '--work N[unit]' and an energy reading in every run ('--energy')",
    }
}

fn declare(name: &str, unit: String, higher_is_better: bool) {
    set_custom_meta_if_absent(
        name,
        CustomMeta {
            unit: Some(unit),
            higher_is_better,
        },
    );
}

/// The inputs of one run
#[derive(Debug, Clone, Copy, Default)]
pub struct RunInputs {
    pub time: f64,
    pub energy: Option<f64>,
    pub instructions: Option<f64>,
    pub cycles: Option<f64>,
}

/// Declares unit and direction of the metrics (`--work` fixes the unit label)
pub fn declare_all(work: Option<&Work>) {
    declare("edp", "J*s".into(), false);
    declare("ed2p", "J*s^2".into(), false);
    if let Some(Work { unit, .. }) = work {
        declare("time_per_op", format!("s/{unit}"), false);
        declare("ops_per_second", format!("{unit}/s"), true);
        declare("energy_per_op", format!("J/{unit}"), false);
        declare("ops_per_joule", format!("{unit}/J"), true);
        declare("edp_per_op", format!("J*s/{unit}^2"), false);
        declare("ed2p_per_op", format!("J*s^2/{unit}^3"), false);
        declare("instructions_per_op", format!("instructions/{unit}"), false);
        declare("cycles_per_op", format!("cycles/{unit}"), false);
    }
}

/// The derived values of one run: only those whose inputs exist. A rate of a zero
/// time or energy is undefined and left out (`None`).
pub fn derive_run(run: &RunInputs, work: Option<&Work>) -> Vec<(&'static str, Option<f64>)> {
    let mut out = Vec::new();
    let t = run.time;
    if let Some(e) = run.energy {
        out.push(("edp", Some(e * t)));
        out.push(("ed2p", Some(e * t * t)));
    }
    if let Some(Work { amount: n, .. }) = work {
        let n = *n;
        out.push(("time_per_op", Some(t / n)));
        out.push(("ops_per_second", (t > 0.0).then(|| n / t)));
        if let Some(e) = run.energy {
            out.push(("energy_per_op", Some(e / n)));
            out.push(("ops_per_joule", (e > 0.0).then(|| n / e)));
            out.push(("edp_per_op", Some((e / n) * (t / n))));
            out.push(("ed2p_per_op", Some((e / n) * (t / n) * (t / n))));
        }
        if let Some(v) = run.instructions {
            out.push(("instructions_per_op", Some(v / n)));
        }
        if let Some(v) = run.cycles {
            out.push(("cycles_per_op", Some(v / n)));
        }
    }
    out
}

/// Adds the derived metrics to a finished (imported) `result`, never replacing a metric of the
/// same name. A series is only added when every run has the inputs it needs.
pub fn augment(result: &mut BenchmarkResult, work: Option<&Work>) {
    let Some(times) = result.times.clone().filter(|t| !t.is_empty()) else {
        return;
    };
    let aligned = |len: usize| len == times.len();
    let energy = result.energy_joules.as_ref().filter(|e| aligned(e.len()));
    let counter = |name: &str| -> Option<&Vec<Option<u64>>> {
        result
            .counters
            .as_ref()?
            .get(name)
            .filter(|c| aligned(c.len()))
    };
    let (instructions, cycles) = (counter("instructions"), counter("cycles"));
    let mut series: BTreeMap<&'static str, Vec<Option<f64>>> = BTreeMap::new();
    for (i, &time) in times.iter().enumerate() {
        let run = RunInputs {
            time,
            energy: energy.map(|e| e[i]),
            instructions: instructions.and_then(|c| c[i]).map(|v| v as f64),
            cycles: cycles.and_then(|c| c[i]).map(|v| v as f64),
        };
        let derived = derive_run(&run, work);
        for (name, value) in derived {
            series.entry(name).or_default().resize(i, None);
            series.entry(name).or_default().push(value);
        }
    }
    declare_all(work);
    let metrics = result.custom_metrics.get_or_insert_with(BTreeMap::new);
    let summary = result
        .custom_metrics_summary
        .get_or_insert_with(BTreeMap::new);
    for (name, mut values) in series {
        values.resize(times.len(), None);
        let present: Vec<f64> = values.iter().flatten().copied().collect();
        if metrics.contains_key(name) {
            continue;
        }
        if let Some(s) = MetricSummary::from_values(&present) {
            summary.insert(name.to_string(), s);
            metrics.insert(name.to_string(), values);
        }
    }
    if metrics.is_empty() {
        result.custom_metrics = None;
    }
    if summary.is_empty() {
        result.custom_metrics_summary = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> BenchmarkResult {
        BenchmarkResult {
            times: Some(vec![2.0, 4.0]),
            energy_joules: Some(vec![10.0, 20.0]),
            counters: Some(BTreeMap::from([
                ("instructions".to_string(), vec![Some(1000), Some(3000)]),
                ("cycles".to_string(), vec![Some(500), Some(1500)]),
            ])),
            ..Default::default()
        }
    }

    #[test]
    fn parses_work() {
        let w: Work = "1000000".parse().unwrap();
        assert_eq!((w.amount, w.unit.as_str()), (1e6, "op"));
        let w: Work = "2.5e3 requests".parse().unwrap();
        assert_eq!((w.amount, w.unit.as_str()), (2500.0, "requests"));
        let w: Work = "10events".parse().unwrap();
        assert_eq!((w.amount, w.unit.as_str()), (10.0, "events"));
        let w: Work = "1_000MB".parse().unwrap();
        assert_eq!((w.amount, w.unit.as_str()), (1000.0, "MB"));
        for bad in ["", "ops", "0", "-3", "1e999", "5 a/b"] {
            assert!(bad.parse::<Work>().is_err(), "{bad}");
        }
    }

    #[test]
    fn edp_and_ed2p_without_work() {
        let mut r = result();
        augment(&mut r, None);
        let m = r.custom_metrics.as_ref().unwrap();
        assert_eq!(m["edp"], vec![Some(20.0), Some(80.0)]);
        assert_eq!(m["ed2p"], vec![Some(40.0), Some(320.0)]);
        assert_eq!(m.len(), 2);
        assert_eq!(r.custom_metrics_summary.as_ref().unwrap()["edp"].mean, 50.0);
        assert_eq!(
            r.primary_mean(crate::metric::Metric::custom("edp")),
            Some(50.0)
        );
    }

    #[test]
    fn per_work_metrics() {
        let mut r = result();
        augment(&mut r, Some(&"100ops".parse().unwrap()));
        let m = r.custom_metrics.as_ref().unwrap();
        assert_eq!(m["time_per_op"], vec![Some(0.02), Some(0.04)]);
        assert_eq!(m["energy_per_op"], vec![Some(0.1), Some(0.2)]);
        assert_eq!(m["ops_per_second"], vec![Some(50.0), Some(25.0)]);
        assert_eq!(m["ops_per_joule"], vec![Some(10.0), Some(5.0)]);
        assert_eq!(m["instructions_per_op"], vec![Some(10.0), Some(30.0)]);
        assert_eq!(m["cycles_per_op"], vec![Some(5.0), Some(15.0)]);
        assert_eq!(m["edp_per_op"][0], Some(0.1 * 0.02));
        let meta = crate::metric::custom_meta("ops_per_joule").unwrap();
        assert!(meta.higher_is_better);
        assert_eq!(meta.unit.as_deref(), Some("ops/J"));
        assert!(crate::metric::Metric::custom("ops_per_joule").higher_is_better());
    }

    #[test]
    fn missing_inputs_add_nothing() {
        let mut r = BenchmarkResult {
            times: Some(vec![1.0, 2.0]),
            ..Default::default()
        };
        augment(&mut r, None);
        assert!(r.custom_metrics.is_none());
        augment(&mut r, Some(&"5ops".parse().unwrap()));
        let m = r.custom_metrics.as_ref().unwrap();
        assert_eq!(m.len(), 2);
        assert!(m.contains_key("time_per_op") && m.contains_key("ops_per_second"));
        // energy of fewer runs than times is not paired
        let mut r = result();
        r.energy_joules = Some(vec![10.0]);
        augment(&mut r, None);
        assert!(r.custom_metrics.is_none());
    }

    #[test]
    fn keeps_a_metric_of_the_same_name() {
        let mut r = result();
        r.custom_metrics = Some(BTreeMap::from([("edp".to_string(), vec![Some(1.0)])]));
        augment(&mut r, None);
        assert_eq!(r.custom_metrics.as_ref().unwrap()["edp"], vec![Some(1.0)]);
    }
}
