//! `--delta-table`: one row per measured dimension (time, CPU time, memory,
//! energy, power) comparing a command with the reference command (#212).
//!
//! Dimensions that were not measured for both commands are left out, so the
//! table degrades to whatever is available (e.g. wall time only).

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::benchmark::relative_speed::fastest_of;
use crate::output::colors;
use crate::stats::{basic, deep, precision::t_975};
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
}

impl Dimension {
    pub const ALL: [Dimension; 6] = [
        Dimension::WallTime,
        Dimension::PeakMemory,
        Dimension::Energy,
        Dimension::Power,
        Dimension::CpuUser,
        Dimension::CpuSystem,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Dimension::WallTime => "Wall time",
            Dimension::CpuUser => "CPU time (user)",
            Dimension::CpuSystem => "CPU time (system)",
            Dimension::PeakMemory => "Peak RSS",
            Dimension::Energy => "Energy (RAPL)",
            Dimension::Power => "Average power",
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
    /// Half-width of the 95% confidence interval of `relative`
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
        _ => None,
    };
    Some(Stat { mean, stddev })
}

/// Two-sided p-value; two constant, different samples are certainly different.
fn p_value(a: &[f64], b: &[f64]) -> Option<f64> {
    if let Some(s) = deep::compare_samples(a, b) {
        return Some(s.p_value);
    }
    let constant = |xs: &[f64]| xs.len() >= 3 && xs.iter().all(|&x| x == xs[0]);
    (constant(a) && constant(b) && a[0] != b[0]).then_some(0.0)
}

/// Half-width of the 95% CI of the difference of means (pooled variance,
/// Student's t with n₁ + n₂ − 2 degrees of freedom), relative to the
/// reference mean.
fn relative_ci(a: &[f64], b: &[f64], reference_mean: f64) -> Option<f64> {
    let (n1, n2) = (a.len(), b.len());
    if n1 + n2 < 3 || n1 == 0 || n2 == 0 || reference_mean <= 0.0 {
        return None;
    }
    let var = |xs: &[f64]| {
        if xs.len() < 2 {
            0.0
        } else {
            basic::standard_deviation(xs, None).powi(2)
        }
    };
    let df = n1 + n2 - 2;
    let pooled = (((n1 - 1) as f64) * var(a) + ((n2 - 1) as f64) * var(b)) / df as f64;
    let se = (pooled * (1.0 / n1 as f64 + 1.0 / n2 as f64)).sqrt();
    let half = t_975(df) * se / reference_mean;
    half.is_finite().then_some(half)
}

/// One row per dimension measured for both commands. Lower is better in every
/// dimension.
pub fn compute_rows(reference: &BenchmarkResult, target: &BenchmarkResult) -> Vec<DeltaRow> {
    Dimension::ALL
        .into_iter()
        .filter_map(|dimension| {
            let (rs, ts) = (stat(dimension, reference)?, stat(dimension, target)?);
            let relative = (rs.mean != 0.0).then(|| (ts.mean - rs.mean) / rs.mean);
            let (a, b) = (dimension.samples(reference), dimension.samples(target));
            let (p_value, ci_half_width) = match (&a, &b) {
                (Some(a), Some(b)) => (p_value(a, b), relative_ci(a, b, rs.mean)),
                _ => (None, None),
            };
            let verdict = match (p_value, relative) {
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
  Wall time          107.5 ms ± 9.6 ms   200.0 ms ± 8.2 ms   +86.0% ± 14.3%  0.007  🔻
  CPU time (user)    53.8 ms             100.0 ms            +86.0%          -
  CPU time (system)  26.9 ms             50.0 ms             +86.0%          -
"#);
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
