//! `--delta-table`: one row per measured dimension (time, CPU time, memory,
//! energy, power) comparing a command with the reference command (#212).
//!
//! Dimensions that were not measured for both commands are left out, so the
//! table degrades to whatever is available (e.g. wall time only).

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::benchmark::relative_speed::fastest_of;
use crate::output::colors;
use crate::stats::basic;
use crate::util::units::format_bytes;

/// Significance level for the verdict column
const ALPHA: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    WallTime,
    CpuUser,
    CpuSystem,
    PeakMemory,
    Energy,
    Power,
    /// Hardware counters (`--counters`, #49)
    Instructions,
    Cycles,
    CacheMisses,
    BranchMisses,
    /// Software events (`--counters=sw`, #220)
    ContextSwitches,
    CpuMigrations,
    PageFaults,
    /// `cycles` / time on CPU (#222). Informational: neither direction is better
    EffectiveFrequency,
}

impl Dimension {
    pub const ALL: [Dimension; 14] = [
        Dimension::WallTime,
        Dimension::PeakMemory,
        Dimension::Energy,
        Dimension::Power,
        Dimension::CpuUser,
        Dimension::CpuSystem,
        Dimension::Instructions,
        Dimension::Cycles,
        Dimension::CacheMisses,
        Dimension::BranchMisses,
        Dimension::ContextSwitches,
        Dimension::CpuMigrations,
        Dimension::PageFaults,
        Dimension::EffectiveFrequency,
    ];

    /// Name of the counter in `BenchmarkResult::counters`
    fn counter_name(self) -> Option<&'static str> {
        match self {
            Dimension::Instructions => Some("instructions"),
            Dimension::Cycles => Some("cycles"),
            Dimension::CacheMisses => Some("cache-misses"),
            Dimension::BranchMisses => Some("branch-misses"),
            Dimension::ContextSwitches => Some("context-switches"),
            Dimension::CpuMigrations => Some("cpu-migrations"),
            Dimension::PageFaults => Some("page-faults"),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dimension::WallTime => "Wall time",
            Dimension::CpuUser => "CPU time (user)",
            Dimension::CpuSystem => "CPU time (system)",
            Dimension::PeakMemory => "Peak RSS",
            Dimension::Energy => "Energy (RAPL)",
            Dimension::Power => "Average power",
            Dimension::Instructions => "Instructions",
            Dimension::Cycles => "CPU cycles",
            Dimension::CacheMisses => "Cache misses",
            Dimension::BranchMisses => "Branch misses",
            Dimension::ContextSwitches => "Context switches",
            Dimension::CpuMigrations => "CPU migrations",
            Dimension::PageFaults => "Page faults",
            Dimension::EffectiveFrequency => "CPU frequency",
        }
    }

    fn format(self, value: f64) -> String {
        match self {
            Dimension::WallTime | Dimension::CpuUser | Dimension::CpuSystem => {
                crate::output::format::format_duration(value, None)
            }
            Dimension::PeakMemory => format_bytes(value.max(0.0).round() as u64),
            Dimension::Energy => format!("{value:.3} J"),
            Dimension::Power => format!("{value:.2} W"),
            Dimension::Instructions
            | Dimension::Cycles
            | Dimension::CacheMisses
            | Dimension::BranchMisses => crate::benchmark::format_count(value),
            Dimension::ContextSwitches | Dimension::CpuMigrations | Dimension::PageFaults => {
                if value < 10.0 {
                    format!("{value:.1}")
                } else {
                    format!("{value:.0}")
                }
            }
            Dimension::EffectiveFrequency => crate::frequency::format_hz(value),
        }
    }

    /// Per-run samples, if every run has one.
    fn samples(self, r: &BenchmarkResult) -> Option<Vec<f64>> {
        match self {
            Dimension::WallTime => r.times.clone(),
            Dimension::CpuUser => r.user_times.clone(),
            Dimension::CpuSystem => r.system_times.clone(),
            Dimension::PeakMemory => r
                .memory_usage_byte
                .as_ref()
                .map(|v| v.iter().map(|&b| b as f64).collect()),
            Dimension::Energy => r.energy_joules.clone(),
            Dimension::Power => None,
            Dimension::Instructions
            | Dimension::Cycles
            | Dimension::CacheMisses
            | Dimension::BranchMisses
            | Dimension::ContextSwitches
            | Dimension::CpuMigrations
            | Dimension::PageFaults => {
                // Only when every run has a value (`null` marks a failed read)
                let values = r.counters.as_ref()?.get(self.counter_name()?)?;
                values.iter().map(|v| v.map(|v| v as f64)).collect()
            }
            // Only the aggregate over the runs long enough to count is exported
            Dimension::EffectiveFrequency => None,
        }
        .filter(|v| !v.is_empty())
    }

    /// The mean, from the samples or, failing that, the stored aggregate.
    fn mean(self, r: &BenchmarkResult) -> Option<f64> {
        if let Some(s) = self.samples(r) {
            return Some(basic::mean(&s));
        }
        match self {
            Dimension::WallTime => Some(r.mean),
            Dimension::CpuUser => Some(r.user),
            Dimension::CpuSystem => Some(r.system),
            Dimension::Energy => r.mean_energy_joules,
            Dimension::Power => r.mean_watts,
            Dimension::PeakMemory => None,
            Dimension::Instructions
            | Dimension::Cycles
            | Dimension::CacheMisses
            | Dimension::BranchMisses
            | Dimension::ContextSwitches
            | Dimension::CpuMigrations
            | Dimension::PageFaults => r
                .counters_summary
                .as_ref()
                .and_then(|s| s.get(self.counter_name()?))
                .map(|s| s.mean),
            Dimension::EffectiveFrequency => r.mean_effective_frequency_hz,
        }
        .filter(|m| m.is_finite())
    }
}

/// Mean and spread of one dimension of one command
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub mean: f64,
    pub stddev: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Improvement,
    Regression,
    /// Not significant
    Noise,
    /// No p-value (too few runs, or no per-run samples)
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeltaRow {
    pub dimension: Dimension,
    pub reference: Stat,
    pub target: Stat,
    /// (target − reference) / reference; `None` if the reference mean is 0
    pub relative: Option<f64>,
    /// Half-width of the 95% Welch confidence interval of the difference of
    /// means, divided by the reference mean. It is not a ratio (delta-method)
    /// interval: the uncertainty of the reference mean in the denominator is
    /// ignored.
    pub ci_half_width: Option<f64>,
    pub p_value: Option<f64>,
    pub verdict: Verdict,
}

fn stat(dimension: Dimension, r: &BenchmarkResult) -> Option<Stat> {
    let mean = dimension.mean(r)?;
    let stddev = match dimension.samples(r) {
        Some(s) if s.len() >= 2 => Some(basic::standard_deviation(&s, Some(mean))),
        // Wall time keeps its stored deviation (it may stem from an import)
        _ if dimension == Dimension::WallTime => r.stddev,
        _ if dimension == Dimension::EffectiveFrequency => r.stddev_effective_frequency_hz,
        _ => None,
    };
    Some(Stat { mean, stddev })
}

/// ln Γ(x) (Lanczos approximation, g = 7)
fn ln_gamma(x: f64) -> f64 {
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    let x = x - 1.0;
    let t = x + 7.5;
    let sum = C[0]
        + C[1..]
            .iter()
            .enumerate()
            .map(|(i, c)| c / (x + i as f64 + 1.0))
            .sum::<f64>();
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// Regularized incomplete beta function I_x(a, b) (continued fraction)
fn inc_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    if x > (a + 1.0) / (a + b + 2.0) {
        return 1.0 - inc_beta(1.0 - x, b, a);
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp() / a;
    let tiny = 1e-300;
    let (mut c, mut d) = (1.0, 1.0 - (a + b) * x / (a + 1.0));
    if d.abs() < tiny {
        d = tiny;
    }
    d = 1.0 / d;
    let mut f = d;
    for m in 1..500 {
        let m = m as f64;
        for numerator in [
            m * (b - m) * x / ((a + 2.0 * m - 1.0) * (a + 2.0 * m)),
            -(a + m) * (a + b + m) * x / ((a + 2.0 * m) * (a + 2.0 * m + 1.0)),
        ] {
            d = 1.0 + numerator * d;
            if d.abs() < tiny {
                d = tiny;
            }
            c = 1.0 + numerator / c;
            if c.abs() < tiny {
                c = tiny;
            }
            d = 1.0 / d;
            f *= c * d;
        }
        if (c * d - 1.0).abs() < 1e-14 {
            break;
        }
    }
    front * f
}

/// Two-sided tail probability P(|T| ≥ |t|) of Student's t with `df` degrees
/// of freedom (`df` may be fractional).
fn t_two_sided_p(t: f64, df: f64) -> f64 {
    inc_beta(df / (df + t * t), df / 2.0, 0.5)
}

/// 97.5% quantile of Student's t with fractional `df` (bisection)
fn t_quantile_975(df: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 1e3);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if t_two_sided_p(mid, df) > ALPHA {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Welch's t-test of `b` against `a`: the two-sided p-value and the
/// half-width of the 95% confidence interval of the difference of means
/// (absolute units). Both come from the same statistic, the
/// Welch-Satterthwaite degrees of freedom and Student's t, so that
/// `p < 0.05` exactly when the interval excludes 0.
///
/// Two constant samples have no variance: they are different with certainty
/// (p = 0, width 0) or identical (p = 1).
fn welch(a: &[f64], b: &[f64]) -> Option<(f64, f64)> {
    let (n1, n2) = (a.len() as f64, b.len() as f64);
    if a.len() < 2 || b.len() < 2 {
        return None;
    }
    let (m1, m2) = (basic::mean(a), basic::mean(b));
    let v1 = basic::standard_deviation(a, Some(m1)).powi(2) / n1;
    let v2 = basic::standard_deviation(b, Some(m2)).powi(2) / n2;
    let se2 = v1 + v2;
    if se2 == 0.0 {
        return Some((if m1 == m2 { 1.0 } else { 0.0 }, 0.0));
    }
    let df = se2 * se2 / (v1 * v1 / (n1 - 1.0) + v2 * v2 / (n2 - 1.0));
    let se = se2.sqrt();
    let p = t_two_sided_p((m2 - m1) / se, df);
    let half = t_quantile_975(df) * se;
    (p.is_finite() && half.is_finite()).then_some((p, half))
}

/// One row per dimension measured for both commands. Lower is better in every
/// dimension except the CPU frequency, which has no verdict.
pub fn compute_rows(reference: &BenchmarkResult, target: &BenchmarkResult) -> Vec<DeltaRow> {
    Dimension::ALL
        .into_iter()
        .filter_map(|dimension| {
            let (rs, ts) = (stat(dimension, reference)?, stat(dimension, target)?);
            let relative = (rs.mean != 0.0).then(|| (ts.mean - rs.mean) / rs.mean);
            let (a, b) = (dimension.samples(reference), dimension.samples(target));
            let (p_value, ci_half_width) = match (&a, &b) {
                (Some(a), Some(b)) => match welch(a, b) {
                    // Relative to the reference mean (not a ratio interval)
                    Some((p, half)) => (Some(p), (rs.mean > 0.0).then(|| half / rs.mean)),
                    None => (None, None),
                },
                _ => (None, None),
            };
            let verdict = match (p_value, relative) {
                // A different clock is context, not a better or worse result
                _ if dimension == Dimension::EffectiveFrequency => Verdict::Unknown,
                (Some(p), Some(rel)) if p < ALPHA && rel < 0.0 => Verdict::Improvement,
                (Some(p), Some(rel)) if p < ALPHA && rel > 0.0 => Verdict::Regression,
                (Some(_), _) => Verdict::Noise,
                _ => Verdict::Unknown,
            };
            Some(DeltaRow {
                dimension,
                reference: rs,
                target: ts,
                relative,
                ci_half_width,
                p_value,
                verdict,
            })
        })
        .collect()
}

/// The reference result (or the fastest one) and the others, in order.
fn split<'a>(
    results: &'a [BenchmarkResult],
    reference: Option<&'a BenchmarkResult>,
) -> Option<(&'a BenchmarkResult, Vec<&'a BenchmarkResult>)> {
    if results.len() < 2 {
        return None;
    }
    let reference = reference.unwrap_or_else(|| fastest_of(results));
    let others = results
        .iter()
        .filter(|r| !std::ptr::eq(*r, reference))
        .filter(|r| !(r.command == reference.command && r.parameters == reference.parameters))
        .collect();
    Some((reference, others))
}

fn fmt_stat(dimension: Dimension, s: &Stat) -> String {
    match s.stddev {
        Some(sd) => format!("{} ± {}", dimension.format(s.mean), dimension.format(sd)),
        None => dimension.format(s.mean),
    }
}

fn fmt_delta(row: &DeltaRow) -> String {
    match (row.relative, row.ci_half_width) {
        (Some(r), Some(ci)) => format!("{:+.1}% ± {:.1}%", r * 100.0, ci * 100.0),
        (Some(r), None) => format!("{:+.1}%", r * 100.0),
        _ => "n/a".to_string(),
    }
}

fn fmt_p(row: &DeltaRow) -> String {
    row.p_value
        .map_or_else(|| "-".to_string(), |p| format!("{p:.3}"))
}

fn marker(v: Verdict) -> &'static str {
    match v {
        Verdict::Improvement => "⚡",
        Verdict::Regression => "🔻",
        Verdict::Noise => "~",
        Verdict::Unknown => "",
    }
}

fn name(r: &BenchmarkResult) -> String {
    format!("'{}'", r.command)
}

/// Terminal tables, one per non-reference command. Empty if there is nothing
/// to compare.
pub fn terminal(results: &[BenchmarkResult], reference: Option<&BenchmarkResult>) -> String {
    render_terminal(results, reference, true)
}

fn render_terminal(
    results: &[BenchmarkResult],
    reference: Option<&BenchmarkResult>,
    color: bool,
) -> String {
    let Some((reference, others)) = split(results, reference) else {
        return String::new();
    };
    let mut out = String::new();
    for target in others {
        let rows = compute_rows(reference, target);
        if rows.is_empty() {
            continue;
        }
        let mut table: Vec<[String; 5]> = vec![[
            "Measurement".into(),
            format!("{} (mean ± σ)", name(reference)),
            format!("{} (mean ± σ)", name(target)),
            "Delta (%)".into(),
            "p-val".into(),
        ]];
        for r in &rows {
            table.push([
                r.dimension.label().into(),
                fmt_stat(r.dimension, &r.reference),
                fmt_stat(r.dimension, &r.target),
                fmt_delta(r),
                fmt_p(r),
            ]);
        }
        let widths: Vec<usize> = (0..5)
            .map(|c| {
                table
                    .iter()
                    .map(|row| row[c].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        out.push_str(&format!(
            "Benchmark comparison (Reference: {})\n",
            name(reference)
        ));
        for (i, row) in table.iter().enumerate() {
            let mut line = String::from("  ");
            for (c, cell) in row.iter().enumerate() {
                let pad = widths[c] - cell.chars().count();
                let text = format!("{cell}{}", " ".repeat(pad));
                if i > 0 && c == 3 && color {
                    line.push_str(&match rows[i - 1].verdict {
                        Verdict::Improvement => colors::green(&text).to_string(),
                        Verdict::Regression => colors::red(&text).to_string(),
                        _ => text,
                    });
                } else {
                    line.push_str(&text);
                }
                line.push_str("  ");
            }
            if i > 0 {
                line.push_str(marker(rows[i - 1].verdict));
            }
            out.push_str(line.trim_end());
            out.push('\n');
            if i == 0 {
                let total: usize = widths.iter().sum::<usize>() + 2 * widths.len();
                out.push_str(&format!("  {}\n", "─".repeat(total + 2)));
            }
        }
        out.push('\n');
    }
    out
}

/// Markdown tables, one per non-reference command.
pub fn markdown(results: &[BenchmarkResult], reference: Option<&BenchmarkResult>) -> String {
    let Some((reference, others)) = split(results, reference) else {
        return String::new();
    };
    let escape = |s: &str| s.replace('|', "\\|");
    let mut out = String::new();
    for target in others {
        let rows = compute_rows(reference, target);
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n### Delta: `{}` vs reference `{}`\n\n",
            escape(&target.command),
            escape(&reference.command)
        ));
        out.push_str(
            "| Measurement | Reference (mean ± σ) | Command (mean ± σ) | Delta (%) | p-val | |\n",
        );
        out.push_str("|:---|---:|---:|---:|---:|:---|\n");
        for r in &rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                r.dimension.label(),
                fmt_stat(r.dimension, &r.reference),
                fmt_stat(r.dimension, &r.target),
                fmt_delta(r),
                fmt_p(r),
                marker(r.verdict)
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(command: &str, times: &[f64]) -> BenchmarkResult {
        let mean = basic::mean(times);
        BenchmarkResult {
            command: command.into(),
            mean,
            stddev: Some(basic::standard_deviation(times, Some(mean))),
            median: mean,
            user: mean / 2.0,
            system: mean / 4.0,
            min: times.iter().cloned().fold(f64::MAX, f64::min),
            max: times.iter().cloned().fold(f64::MIN, f64::max),
            times: Some(times.to_vec()),
            ..Default::default()
        }
    }

    fn full(command: &str, scale: f64) -> BenchmarkResult {
        let times: Vec<f64> = [0.0142, 0.0145, 0.0139, 0.0143, 0.0141, 0.0144]
            .iter()
            .map(|t| t * scale)
            .collect();
        let mut r = result(command, &times);
        r.user_times = Some(times.iter().map(|t| t * 0.5).collect());
        r.system_times = Some(times.iter().map(|t| t * 0.25).collect());
        let mem = [
            8_400_000u64,
            8_500_000,
            8_300_000,
            8_400_000,
            8_450_000,
            8_350_000,
        ];
        r.memory_usage_byte = Some(mem.iter().map(|m| (*m as f64 * scale) as u64).collect());
        let energy: Vec<f64> = [1.45, 1.40, 1.50, 1.44, 1.46, 1.43]
            .iter()
            .map(|e| e * scale)
            .collect();
        r.mean_energy_joules = Some(basic::mean(&energy));
        r.mean_watts = Some(100.0 * scale.sqrt());
        r.energy_joules = Some(energy);
        r
    }

    #[test]
    fn delta_is_relative_to_the_reference() {
        let rows = compute_rows(&full("a", 1.0), &full("b", 0.5));
        let wall = &rows[0];
        assert_eq!(wall.dimension, Dimension::WallTime);
        assert!((wall.relative.unwrap() + 0.5).abs() < 1e-9);
        assert_eq!(wall.verdict, Verdict::Improvement);
        assert!(wall.ci_half_width.unwrap() > 0.0);
        assert_eq!(rows.len(), 6);
    }

    // Reference values computed independently with SciPy 1.x
    // (`scipy.stats.ttest_ind(b, a, equal_var=False)`):
    //   a = [10.0, 10.2, 9.9, 10.1, 10.3, 9.8], b = [9.0, 9.4, 8.8, 9.1, 9.6, 8.9, 9.2, 9.0]
    //   df = 11.982014403689236, t = -7.636386686301188, p = 6.0883526276694205e-06
    //   95% CI of mean(b) - mean(a): (-1.188964820799797, -0.661035179200201)
    //   half-width / mean(a) (= 10.05) = 0.02626515629848736
    #[test]
    fn welch_ci_and_p_value_match_scipy() {
        let a = [10.0, 10.2, 9.9, 10.1, 10.3, 9.8];
        let b = [9.0, 9.4, 8.8, 9.1, 9.6, 8.9, 9.2, 9.0];
        let (p, half) = welch(&a, &b).unwrap();
        assert!((p / 6.0883526276694205e-6 - 1.0).abs() < 1e-6, "p = {p}");
        assert!((half - 0.263_964_82).abs() < 1e-6, "half = {half}");
        let rows = compute_rows(&result("a", &a), &result("b", &b));
        let wall = &rows[0];
        assert!((wall.p_value.unwrap() / 6.0883526276694205e-6 - 1.0).abs() < 1e-6);
        assert!((wall.ci_half_width.unwrap() - 0.02626515629848736).abs() < 1e-9);
    }

    #[test]
    fn welch_p_value_and_ci_agree() {
        // p < 0.05 exactly when the CI excludes 0
        let a = [1.0, 1.1, 0.9, 1.05, 0.95];
        for shift in [0.0, 0.02, 0.05, 0.1, 0.2, 0.5] {
            let b: Vec<f64> = [1.0, 1.2, 0.8, 1.1, 0.9, 1.0]
                .iter()
                .map(|x| x + shift)
                .collect();
            let (p, half) = welch(&a, &b).unwrap();
            let diff = (basic::mean(&b) - basic::mean(&a)).abs();
            assert_eq!(p < ALPHA, diff > half, "shift {shift}: p {p}, half {half}");
        }
    }

    #[test]
    fn welch_constant_samples() {
        assert_eq!(welch(&[1.0; 4], &[1.0; 4]), Some((1.0, 0.0)));
        assert_eq!(welch(&[1.0; 4], &[2.0; 4]), Some((0.0, 0.0)));
        assert_eq!(welch(&[1.0], &[2.0, 3.0]), None);
    }

    #[test]
    fn regression_and_noise() {
        let rows = compute_rows(&full("a", 1.0), &full("b", 2.0));
        assert!(rows
            .iter()
            .take(3)
            .all(|r| r.verdict == Verdict::Regression));
        let rows = compute_rows(&full("a", 1.0), &full("b", 1.0));
        assert!(rows
            .iter()
            .filter(|r| r.dimension != Dimension::Power)
            .all(|r| r.verdict == Verdict::Noise));
    }

    #[test]
    fn missing_dimensions_are_left_out() {
        let a = result("a", &[0.1, 0.11, 0.12, 0.1]);
        let b = result("b", &[0.2, 0.21, 0.2, 0.19]);
        let dims: Vec<_> = compute_rows(&a, &b).iter().map(|r| r.dimension).collect();
        assert_eq!(
            dims,
            [
                Dimension::WallTime,
                Dimension::CpuUser,
                Dimension::CpuSystem
            ]
        );
        // Energy on one side only
        let dims: Vec<_> = compute_rows(&full("a", 1.0), &b)
            .iter()
            .map(|r| r.dimension)
            .collect();
        assert!(!dims.contains(&Dimension::Energy));
        assert!(!dims.contains(&Dimension::PeakMemory));
    }

    #[test]
    fn mean_only_dimension_has_no_p_value() {
        let mut a = full("a", 1.0);
        let mut b = full("b", 0.5);
        a.energy_joules = None;
        b.energy_joules = None;
        let rows = compute_rows(&a, &b);
        let energy = rows
            .iter()
            .find(|r| r.dimension == Dimension::Energy)
            .unwrap();
        assert!(energy.p_value.is_none());
        assert_eq!(energy.verdict, Verdict::Unknown);
        assert!(energy.relative.is_some());
    }

    #[test]
    fn zero_reference_mean_has_no_delta() {
        let mut a = full("a", 1.0);
        a.mean_watts = Some(0.0);
        let rows = compute_rows(&a, &full("b", 1.0));
        let power = rows
            .iter()
            .find(|r| r.dimension == Dimension::Power)
            .unwrap();
        assert_eq!(power.relative, None);
        assert_eq!(fmt_delta(power), "n/a");
    }

    #[test]
    fn too_few_results_give_no_table() {
        assert_eq!(render_terminal(&[full("a", 1.0)], None, false), "");
        assert_eq!(markdown(&[], None), "");
    }

    #[test]
    fn terminal_snapshot() {
        let results = [full("cmd_a", 1.0), full("cmd_b", 0.74)];
        insta::assert_snapshot!(render_terminal(&results, Some(&results[0]), false), @r#"
Benchmark comparison (Reference: 'cmd_a')
  Measurement        'cmd_a' (mean ± σ)  'cmd_b' (mean ± σ)  Delta (%)      p-val
  ───────────────────────────────────────────────────────────────────────────────────
  Wall time          14.2 ms ± 216.0 µs  10.5 ms ± 159.9 µs  -26.0% ± 1.7%  0.000  ⚡
  Peak RSS           8.0 MB ± 69.1 KB    5.9 MB ± 51.1 KB    -26.0% ± 1.0%  0.000  ⚡
  Energy (RAPL)      1.447 J ± 0.033 J   1.071 J ± 0.025 J   -26.0% ± 2.6%  0.000  ⚡
  Average power      100.00 W            86.02 W             -14.0%         -
  CPU time (user)    7.1 ms ± 108.0 µs   5.3 ms ± 79.9 µs    -26.0% ± 1.7%  0.000  ⚡
  CPU time (system)  3.6 ms ± 54.0 µs    2.6 ms ± 40.0 µs    -26.0% ± 1.7%  0.000  ⚡
"#);
    }

    #[test]
    fn terminal_snapshot_without_samples_for_cpu_time() {
        let results = [
            result("cmd_a", &[0.1, 0.11, 0.12, 0.1]),
            result("cmd_b", &[0.2, 0.21, 0.2, 0.19]),
        ];
        insta::assert_snapshot!(render_terminal(&results, Some(&results[0]), false), @r#"
Benchmark comparison (Reference: 'cmd_a')
  Measurement        'cmd_a' (mean ± σ)  'cmd_b' (mean ± σ)  Delta (%)       p-val
  ────────────────────────────────────────────────────────────────────────────────────
  Wall time          107.5 ms ± 9.6 ms   200.0 ms ± 8.2 ms   +86.0% ± 14.4%  0.000  🔻
  CPU time (user)    53.8 ms             100.0 ms            +86.0%          -
  CPU time (system)  26.9 ms             50.0 ms             +86.0%          -
"#);
    }

    #[test]
    fn counter_rows_appear_only_when_both_have_counters() {
        let counters = |scale: f64| {
            let mut m = std::collections::BTreeMap::new();
            let v: Vec<Option<u64>> = [1000u64, 1010, 990, 1005, 995, 1000]
                .iter()
                .map(|v| Some((*v as f64 * scale) as u64))
                .collect();
            m.insert("instructions".to_string(), v);
            m
        };
        let mut a = full("a", 1.0);
        let mut b = full("b", 1.0);
        let none = compute_rows(&a, &b);
        assert!(none.iter().all(|r| r.dimension != Dimension::Instructions));
        a.counters = Some(counters(1.0));
        assert!(compute_rows(&a, &b)
            .iter()
            .all(|r| r.dimension != Dimension::Instructions));
        b.counters = Some(counters(2.0));
        let rows = compute_rows(&a, &b);
        let row = rows
            .iter()
            .find(|r| r.dimension == Dimension::Instructions)
            .expect("instructions row");
        assert!((row.relative.unwrap() - 1.0).abs() < 0.01);
        assert_eq!(row.verdict, Verdict::Regression);
        assert_eq!(Dimension::Instructions.label(), "Instructions");
    }

    #[test]
    fn software_and_frequency_rows() {
        let counters = |name: &str, base: u64| {
            let v: Vec<Option<u64>> = [0u64, 1, 0, 2, 1, 0]
                .iter()
                .map(|v| Some(v + base))
                .collect();
            std::collections::BTreeMap::from([(name.to_string(), v)])
        };
        let mut a = full("a", 1.0);
        let mut b = full("b", 1.0);
        a.counters = Some(counters("cpu-migrations", 0));
        b.counters = Some(counters("cpu-migrations", 10));
        a.mean_effective_frequency_hz = Some(3.4e9);
        a.stddev_effective_frequency_hz = Some(1.0e8);
        b.mean_effective_frequency_hz = Some(2.9e9);
        let rows = compute_rows(&a, &b);
        let mig = rows
            .iter()
            .find(|r| r.dimension == Dimension::CpuMigrations)
            .expect("migrations row");
        assert_eq!(mig.verdict, Verdict::Regression);
        assert_eq!(Dimension::CpuMigrations.format(1.0 / 3.0), "0.3");
        assert_eq!(Dimension::ContextSwitches.format(123.4), "123");
        let freq = rows
            .iter()
            .find(|r| r.dimension == Dimension::EffectiveFrequency)
            .expect("frequency row");
        assert_eq!(freq.reference.mean, 3.4e9);
        assert_eq!(freq.reference.stddev, Some(1.0e8));
        assert_eq!(freq.target.stddev, None);
        assert!((freq.relative.unwrap() + 0.147).abs() < 0.001);
        // a lower clock is not a "result": no verdict
        assert_eq!(freq.verdict, Verdict::Unknown);
        assert_eq!(
            fmt_stat(Dimension::EffectiveFrequency, &freq.reference),
            "3.40 GHz ± 100 MHz"
        );
        // not measured for both: no row
        assert!(!compute_rows(&full("a", 1.0), &b)
            .iter()
            .any(|r| r.dimension == Dimension::EffectiveFrequency));
    }

    #[test]
    fn markdown_snapshot() {
        let results = [full("cmd_a", 1.0), full("cmd_b", 0.74)];
        insta::assert_snapshot!(markdown(&results, Some(&results[0])), @r#"

### Delta: `cmd_b` vs reference `cmd_a`

| Measurement | Reference (mean ± σ) | Command (mean ± σ) | Delta (%) | p-val | |
|:---|---:|---:|---:|---:|:---|
| Wall time | 14.2 ms ± 216.0 µs | 10.5 ms ± 159.9 µs | -26.0% ± 1.7% | 0.000 | ⚡ |
| Peak RSS | 8.0 MB ± 69.1 KB | 5.9 MB ± 51.1 KB | -26.0% ± 1.0% | 0.000 | ⚡ |
| Energy (RAPL) | 1.447 J ± 0.033 J | 1.071 J ± 0.025 J | -26.0% ± 2.6% | 0.000 | ⚡ |
| Average power | 100.00 W | 86.02 W | -14.0% | - |  |
| CPU time (user) | 7.1 ms ± 108.0 µs | 5.3 ms ± 79.9 µs | -26.0% ± 1.7% | 0.000 | ⚡ |
| CPU time (system) | 3.6 ms ± 54.0 µs | 2.6 ms ± 40.0 µs | -26.0% ± 1.7% | 0.000 | ⚡ |
"#);
    }
}
