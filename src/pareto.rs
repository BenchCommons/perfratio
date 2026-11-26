//! `--pareto`, `--constraint`, `--objective`: Pareto frontier and
//! constraint-aware comparison (#326, `docs/specs/pareto.md`).

use std::fmt::Write as _;

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::metric::Metric;
use crate::output::format::format_duration;
use crate::stats::summary::percentile;

/// Which way a metric improves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    LowerIsBetter,
    HigherIsBetter,
}

/// Direction of a metric. Every built-in metric improves downwards; a custom metric follows
/// its descriptor (`--output-metric NAME[unit,higher]=REGEX`, #317).
pub fn direction(metric: Metric) -> Direction {
    if metric.higher_is_better() {
        Direction::HigherIsBetter
    } else {
        Direction::LowerIsBetter
    }
}

/// Statistic of the per-run samples that represents a command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Statistic {
    Mean,
    Median,
    Min,
    Max,
    /// Percentile, 0 to 100
    Percentile(f64),
}

impl Statistic {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "mean" => Some(Self::Mean),
            "median" => Some(Self::Median),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            _ => {
                let p: f64 = s.strip_prefix('p')?.parse().ok()?;
                (0.0..=100.0).contains(&p).then_some(Self::Percentile(p))
            }
        }
    }

    fn name(self) -> String {
        match self {
            Self::Mean => "mean".into(),
            Self::Median => "median".into(),
            Self::Min => "min".into(),
            Self::Max => "max".into(),
            Self::Percentile(p) => format!("p{p}"),
        }
    }
}

/// A metric with the statistic taken from it, written `wall`, `wall.p95`, `memory.max`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Axis {
    pub metric: Metric,
    pub statistic: Statistic,
}

impl Axis {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (metric_part, statistic) = match s.rsplit_once('.') {
            Some((m, stat)) if Statistic::parse(&stat.to_ascii_lowercase()).is_some() => (
                m,
                Statistic::parse(&stat.to_ascii_lowercase()).unwrap_or(Statistic::Mean),
            ),
            Some((_, stat)) if stat.starts_with('p') || stat.starts_with("me") => {
                return Err(format!(
                    "invalid statistic '{stat}' in '{s}': use mean, median, min, max or pNN (0-100)"
                ))
            }
            _ => (s, Statistic::Mean),
        };
        let metric = metric_part.parse::<Metric>()?;
        Ok(Self { metric, statistic })
    }

    /// Name as written, e.g. `wall` or `wall.p95`
    pub fn name(&self) -> String {
        match self.statistic {
            Statistic::Mean => self.metric.to_string(),
            stat => format!("{}.{}", self.metric, stat.name()),
        }
    }

    /// Value of the axis for a result; `None` if the result has no data for it
    /// (an unavailable metric is never treated as 0).
    pub fn value(&self, result: &BenchmarkResult) -> Option<f64> {
        let value = match self.statistic {
            Statistic::Mean => result.primary_mean(self.metric)?,
            stat => {
                let mut samples = result.primary_samples(self.metric)?;
                samples.retain(|v| v.is_finite());
                if samples.is_empty() {
                    return None;
                }
                samples.sort_by(f64::total_cmp);
                match stat {
                    Statistic::Median => percentile(&samples, 50.0)?,
                    Statistic::Min => samples[0],
                    Statistic::Max => samples[samples.len() - 1],
                    Statistic::Percentile(p) => percentile(&samples, p)?,
                    Statistic::Mean => unreachable!("handled above"),
                }
            }
        };
        value.is_finite().then_some(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Le,
    Lt,
    Ge,
    Gt,
}

impl Op {
    fn holds(self, value: f64, limit: f64) -> bool {
        match self {
            Op::Le => value <= limit,
            Op::Lt => value < limit,
            Op::Ge => value >= limit,
            Op::Gt => value > limit,
        }
    }
}

/// `--constraint 'memory <= 256MiB'`
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub axis: Axis,
    pub op: Op,
    /// In the base unit of the metric (seconds, bytes, joules, plain count)
    pub value: f64,
    /// As the user wrote it, trimmed
    pub text: String,
}

impl Constraint {
    pub fn parse(s: &str) -> Result<Self, String> {
        let text = s.trim().to_string();
        // two-character operators first
        let (pos, op, len) = [("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)]
            .iter()
            .filter_map(|(sym, op)| text.find(sym).map(|p| (p, *op, sym.len())))
            .min_by_key(|(p, _, len)| (*p, usize::MAX - len))
            .ok_or_else(|| {
                format!("invalid constraint '{text}': expected '<METRIC> <op> <VALUE>' with <=, <, >= or >")
            })?;
        let axis =
            Axis::parse(&text[..pos]).map_err(|e| format!("invalid constraint '{text}': {e}"))?;
        let value = parse_quantity(axis.metric, text[pos + len..].trim())
            .map_err(|e| format!("invalid constraint '{text}': {e}"))?;
        Ok(Self {
            axis,
            op,
            value,
            text,
        })
    }
}

/// Parse a number with an optional unit, into the base unit of `metric`.
pub(crate) fn parse_quantity(metric: Metric, s: &str) -> Result<f64, String> {
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("'{s}' is not a number with an optional unit"))?;
    if !number.is_finite() {
        return Err(format!("'{s}' is not finite"));
    }
    let unit = unit.trim();
    if unit.is_empty() {
        return Ok(number);
    }
    let table: &[(&str, f64)] = if metric.is_time() {
        &[
            ("ns", 1e-9),
            ("us", 1e-6),
            ("µs", 1e-6),
            ("ms", 1e-3),
            ("s", 1.0),
            ("min", 60.0),
        ]
    } else if metric == Metric::Memory {
        &[
            ("B", 1.0),
            ("KB", 1e3),
            ("KiB", 1024.0),
            ("MB", 1e6),
            ("MiB", 1024.0 * 1024.0),
            ("GB", 1e9),
            ("GiB", 1024.0 * 1024.0 * 1024.0),
        ]
    } else if metric == Metric::Energy {
        &[("uJ", 1e-6), ("mJ", 1e-3), ("J", 1.0), ("kJ", 1e3)]
    } else {
        &[]
    };
    table
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, factor)| number * factor)
        .ok_or_else(|| {
            if table.is_empty() {
                format!("metric '{metric}' takes a plain number, got unit '{unit}'")
            } else {
                format!(
                    "unknown unit '{unit}' for metric '{metric}' (one of {})",
                    table.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                )
            }
        })
}

/// `--objective minimize:energy`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Objective {
    pub direction: Direction,
    pub axis: Axis,
}

impl Objective {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (dir, axis) = s.split_once(':').ok_or_else(|| {
            format!("invalid objective '{s}': expected minimize:<METRIC> or maximize:<METRIC>")
        })?;
        let direction = match dir.trim().to_ascii_lowercase().as_str() {
            "minimize" | "min" => Direction::LowerIsBetter,
            "maximize" | "max" => Direction::HigherIsBetter,
            other => {
                return Err(format!(
                    "invalid objective '{s}': '{other}' is not minimize or maximize"
                ))
            }
        };
        Ok(Self {
            direction,
            axis: Axis::parse(axis).map_err(|e| format!("invalid objective '{s}': {e}"))?,
        })
    }
}

/// Settings of `--pareto`, `--constraint`, `--objective` and `--noise-threshold`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParetoConfig {
    pub axes: Vec<Axis>,
    pub constraints: Vec<Constraint>,
    pub objective: Option<Objective>,
    /// Relative noise threshold (0.05 = 5%); 0 compares the values strictly
    pub noise_threshold: f64,
}

impl ParetoConfig {
    /// Build the configuration from the raw option values; `None` when none of
    /// `--pareto`, `--constraint` and `--objective` is given.
    pub fn parse<'a>(
        pareto: Option<&str>,
        constraints: impl IntoIterator<Item = &'a str>,
        objective: Option<&str>,
        noise_threshold: Option<&str>,
    ) -> Result<Option<Self>, String> {
        let constraints: Vec<Constraint> = constraints
            .into_iter()
            .map(Constraint::parse)
            .collect::<Result<_, _>>()?;
        let objective = objective.map(Objective::parse).transpose()?;
        let mut axes: Vec<Axis> = Vec::new();
        if let Some(list) = pareto {
            for item in list.split(',') {
                let axis = Axis::parse(item)?;
                if axes.iter().any(|a| a.name() == axis.name()) {
                    return Err(format!("duplicate metric '{}' in --pareto", axis.name()));
                }
                axes.push(axis);
            }
        }
        let noise_threshold = match noise_threshold {
            Some(s) => {
                let value: f64 = s
                    .trim()
                    .trim_end_matches('%')
                    .parse()
                    .ok()
                    .filter(|v: &f64| v.is_finite() && *v >= 0.0)
                    .ok_or_else(|| {
                        format!("invalid --noise-threshold '{s}': expected a percentage >= 0")
                    })?;
                if pareto.is_none() {
                    return Err("--noise-threshold needs --pareto".into());
                }
                value / 100.0
            }
            None => 0.0,
        };
        if axes.is_empty() {
            match (&objective, constraints.is_empty()) {
                (None, true) => return Ok(None),
                (None, false) => {
                    return Err("--constraint needs --pareto or --objective".into());
                }
                (Some(o), _) => axes.push(o.axis),
            }
        }
        Ok(Some(Self {
            axes,
            constraints,
            objective,
            noise_threshold,
        }))
    }

    /// The direction of the axis `i` of the frontier.
    fn directions(&self) -> Vec<Direction> {
        self.axes
            .iter()
            .map(|a| {
                match &self.objective {
                    // an explicit objective on the same axis overrides the default direction
                    Some(o) if o.axis == *a => o.direction,
                    _ => direction(a.metric),
                }
            })
            .collect()
    }
}

/// Why a command is not a candidate.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Excluded {
    pub command_index: usize,
    /// `metric_unavailable` or `constraint_violated`
    pub reason: &'static str,
    /// The metric without data, or the violated constraint as written
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dominated {
    pub command_index: usize,
    pub by: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObjectiveOutcome {
    pub direction: Direction,
    pub metric: String,
    /// `None` when no command is a candidate
    pub best: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// The `pareto` object of the JSON export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analysis {
    pub metrics: Vec<AxisInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_threshold: Option<f64>,
    pub frontier: Vec<usize>,
    pub dominated: Vec<Dominated>,
    /// On the frontier, but strictly dominated by a command within the noise threshold
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub within_noise: Vec<usize>,
    pub excluded: Vec<Excluded>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<ObjectiveOutcome>,
    /// Value of every axis for each command, in `metrics` order; only for the
    /// terminal and Markdown tables
    #[serde(skip)]
    pub values: Vec<Vec<Option<f64>>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AxisInfo {
    pub metric: String,
    pub direction: Direction,
}

/// Split of `candidates` (rows of oriented axis values) into the Pareto frontier and
/// the dominated points. Pure, for unit tests.
///
/// `a` dominates `b` if it is not worse on any axis and better on at least one, where
/// "better" and "worse" need a relative difference above `noise` (0 = strict).
/// Returns, per candidate: its dominators (beyond noise) and its strict dominators.
fn dominators(values: &[&[f64]], dirs: &[Direction], noise: f64) -> Vec<(Vec<usize>, Vec<usize>)> {
    // +1: a is better than b, -1: worse, 0: tie within `noise`
    let cmp = |a: f64, b: f64, dir: Direction, noise: f64| -> i8 {
        let diff = match dir {
            Direction::LowerIsBetter => b - a,
            Direction::HigherIsBetter => a - b,
        };
        let margin = noise * a.abs().max(b.abs());
        if diff > margin {
            1
        } else if diff < -margin {
            -1
        } else {
            0
        }
    };
    let dominates = |a: &[f64], b: &[f64], noise: f64| -> bool {
        let mut better = false;
        for ((x, y), dir) in a.iter().zip(b).zip(dirs) {
            match cmp(*x, *y, *dir, noise) {
                -1 => return false,
                1 => better = true,
                _ => {}
            }
        }
        better
    };
    (0..values.len())
        .map(|b| {
            let by = |n: f64| -> Vec<usize> {
                (0..values.len())
                    .filter(|&a| a != b && dominates(values[a], values[b], n))
                    .collect()
            };
            (by(noise), if noise > 0.0 { by(0.0) } else { Vec::new() })
        })
        .collect()
}

/// Analyse the results. Command indices refer to positions in `results`.
pub fn analyze(config: &ParetoConfig, results: &[BenchmarkResult]) -> Analysis {
    let dirs = config.directions();
    let mut excluded = Vec::new();
    let mut candidates: Vec<usize> = Vec::new();
    let mut values: Vec<Vec<Option<f64>>> = Vec::with_capacity(results.len());
    let mut candidate_values: Vec<Vec<f64>> = Vec::new();
    let mut objective_values: Vec<f64> = Vec::new();

    for (index, result) in results.iter().enumerate() {
        let axis_values: Vec<Option<f64>> = config.axes.iter().map(|a| a.value(result)).collect();
        let mut reason: Option<(&'static str, String)> = None;
        for c in &config.constraints {
            match c.axis.value(result) {
                None => {
                    reason = Some(("metric_unavailable", c.axis.name()));
                    break;
                }
                Some(v) if !c.op.holds(v, c.value) => {
                    reason = Some(("constraint_violated", c.text.clone()));
                    break;
                }
                Some(_) => {}
            }
        }
        if reason.is_none() {
            if let Some(i) = axis_values.iter().position(Option::is_none) {
                reason = Some(("metric_unavailable", config.axes[i].name()));
            }
        }
        let objective_value = config.objective.and_then(|o| o.axis.value(result));
        if reason.is_none() && config.objective.is_some() && objective_value.is_none() {
            let name = config.objective.map(|o| o.axis.name()).unwrap_or_default();
            reason = Some(("metric_unavailable", name));
        }
        match reason {
            Some((reason, detail)) => excluded.push(Excluded {
                command_index: index,
                reason,
                detail,
            }),
            None => {
                candidates.push(index);
                candidate_values.push(axis_values.iter().map(|v| v.unwrap_or(0.0)).collect());
                objective_values.push(objective_value.unwrap_or(0.0));
            }
        }
        values.push(axis_values);
    }

    let rows: Vec<&[f64]> = candidate_values.iter().map(Vec::as_slice).collect();
    let doms = dominators(&rows, &dirs, config.noise_threshold);
    let mut frontier = Vec::new();
    let mut dominated = Vec::new();
    let mut within_noise = Vec::new();
    for (pos, (by, strict)) in doms.iter().enumerate() {
        let index = candidates[pos];
        if by.is_empty() {
            frontier.push(index);
            if !strict.is_empty() {
                within_noise.push(index);
            }
        } else {
            dominated.push(Dominated {
                command_index: index,
                by: by.iter().map(|&p| candidates[p]).collect(),
            });
        }
    }

    let objective = config.objective.map(|o| {
        let mut best: Option<(usize, f64)> = None;
        for (pos, &v) in objective_values.iter().enumerate() {
            let better = match (best, o.direction) {
                (None, _) => true,
                (Some((_, b)), Direction::LowerIsBetter) => v < b,
                (Some((_, b)), Direction::HigherIsBetter) => v > b,
            };
            if better {
                best = Some((pos, v));
            }
        }
        ObjectiveOutcome {
            direction: o.direction,
            metric: o.axis.name(),
            best: best.map(|(pos, _)| candidates[pos]),
            value: best.map(|(_, v)| v),
        }
    });

    Analysis {
        metrics: config
            .axes
            .iter()
            .zip(&dirs)
            .map(|(a, d)| AxisInfo {
                metric: a.name(),
                direction: *d,
            })
            .collect(),
        constraints: config.constraints.iter().map(|c| c.text.clone()).collect(),
        noise_threshold: (config.noise_threshold > 0.0).then_some(config.noise_threshold),
        frontier,
        dominated,
        within_noise,
        excluded,
        objective,
        values,
    }
}

pub(crate) fn format_value(metric: Metric, value: f64) -> String {
    match metric {
        m if m.is_time() => format_duration(value, None),
        Metric::Energy => format!("{value:.3} J"),
        Metric::Memory => format!("{value:.0} B"),
        Metric::Instructions | Metric::Cycles => format!("{value:.0}"),
        _ => format!("{value:.4}"),
    }
}

fn status(analysis: &Analysis, results: &[BenchmarkResult], index: usize) -> String {
    let name = |i: usize| results[i].command.clone();
    if let Some(e) = analysis.excluded.iter().find(|e| e.command_index == index) {
        return match e.reason {
            "constraint_violated" => format!("excluded: violates '{}'", e.detail),
            _ => format!("excluded: no data for {}", e.detail),
        };
    }
    if let Some(d) = analysis.dominated.iter().find(|d| d.command_index == index) {
        let by: Vec<String> = d.by.iter().map(|&i| name(i)).collect();
        return format!("dominated by {}", by.join(", "));
    }
    let mut text = if analysis.within_noise.contains(&index) {
        "not dominated (within noise)".to_string()
    } else {
        "frontier".to_string()
    };
    if analysis.objective.as_ref().and_then(|o| o.best) == Some(index) {
        text.push_str(", best objective");
    }
    text
}

fn header_line(analysis: &Analysis) -> String {
    let metrics: Vec<String> = analysis
        .metrics
        .iter()
        .map(|m| {
            let arrow = match m.direction {
                Direction::LowerIsBetter => "↓",
                Direction::HigherIsBetter => "↑",
            };
            format!("{}{arrow}", m.metric)
        })
        .collect();
    let mut line = format!("Pareto frontier over {}", metrics.join(", "));
    if !analysis.constraints.is_empty() {
        let _ = write!(
            line,
            " with constraints {}",
            analysis.constraints.join("; ")
        );
    }
    line
}

fn objective_line(analysis: &Analysis, results: &[BenchmarkResult]) -> Option<String> {
    let o = analysis.objective.as_ref()?;
    let verb = match o.direction {
        Direction::LowerIsBetter => "minimize",
        Direction::HigherIsBetter => "maximize",
    };
    Some(match o.best {
        Some(i) => format!("Objective {verb} {}: {}", o.metric, results[i].command),
        None => format!(
            "Objective {verb} {}: no command satisfies the constraints",
            o.metric
        ),
    })
}

fn cells(
    config: &ParetoConfig,
    analysis: &Analysis,
    results: &[BenchmarkResult],
) -> Vec<Vec<String>> {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut row = vec![r.command.clone()];
            for (axis, value) in config.axes.iter().zip(&analysis.values[i]) {
                row.push(match value {
                    Some(v) => format_value(axis.metric, *v),
                    None => "n/a".into(),
                });
            }
            row.push(status(analysis, results, i));
            row
        })
        .collect()
}

/// Terminal section; empty without results.
pub fn terminal(config: &ParetoConfig, analysis: &Analysis, results: &[BenchmarkResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut table = vec![{
        let mut head = vec!["Command".to_string()];
        head.extend(config.axes.iter().map(Axis::name));
        head.push("Status".into());
        head
    }];
    table.extend(cells(config, analysis, results));
    let columns = table[0].len();
    let widths: Vec<usize> = (0..columns)
        .map(|c| {
            table
                .iter()
                .map(|r| r[c].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = format!("{}\n", header_line(analysis));
    for (n, row) in table.iter().enumerate() {
        let mut line = String::from("  ");
        for (c, cell) in row.iter().enumerate() {
            line.push_str(cell);
            line.push_str(&" ".repeat(widths[c] - cell.chars().count() + 2));
        }
        out.push_str(line.trim_end());
        out.push('\n');
        if n == 0 {
            out.push_str(&format!(
                "  {}\n",
                "─".repeat(widths.iter().sum::<usize>() + 2 * columns)
            ));
        }
    }
    if let Some(line) = objective_line(analysis, results) {
        let _ = writeln!(out, "  {line}");
    }
    out.push('\n');
    out
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|")
}

/// Markdown section appended to the Markdown export.
pub fn markdown(config: &ParetoConfig, analysis: &Analysis, results: &[BenchmarkResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = format!("\n### {}\n\n", escape_md(&header_line(analysis)));
    let mut head = vec!["Command".to_string()];
    head.extend(config.axes.iter().map(Axis::name));
    head.push("Status".into());
    let _ = writeln!(out, "| {} |", head.join(" | "));
    let mut aligns = vec![":---"];
    aligns.extend(config.axes.iter().map(|_| "---:"));
    aligns.push(":---");
    let _ = writeln!(out, "|{}|", aligns.join("|"));
    for row in cells(config, analysis, results) {
        let row: Vec<String> = row.iter().map(|c| escape_md(c)).collect();
        let _ = writeln!(out, "| {} |", row.join(" | "));
    }
    if let Some(line) = objective_line(analysis, results) {
        let _ = write!(out, "\n{}\n", escape_md(&line));
    }
    out
}

/// Marker for the summary table of the HTML export: `★ frontier` or nothing.
pub fn is_on_frontier(analysis: &Analysis, index: usize) -> bool {
    analysis.frontier.contains(&index)
}

/// HTML section (escaped by the caller-supplied `escape`).
pub fn html(
    config: &ParetoConfig,
    analysis: &Analysis,
    results: &[BenchmarkResult],
    escape: &dyn Fn(&str) -> String,
) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "<h2>{}</h2>\n<table><thead><tr><th>Command</th>",
        escape(&header_line(analysis))
    );
    for axis in &config.axes {
        let _ = write!(out, "<th>{}</th>", escape(&axis.name()));
    }
    out.push_str("<th>Status</th></tr></thead><tbody>");
    for (i, row) in cells(config, analysis, results).iter().enumerate() {
        let class = if is_on_frontier(analysis, i) {
            " class=\"frontier\""
        } else {
            ""
        };
        let _ = write!(out, "<tr{class}>");
        for (c, cell) in row.iter().enumerate() {
            if c == 0 {
                let _ = write!(out, "<td><code>{}</code></td>", escape(cell));
            } else {
                let _ = write!(out, "<td>{}</td>", escape(cell));
            }
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>\n");
    if let Some(line) = objective_line(analysis, results) {
        let _ = writeln!(out, "<p>{}</p>", escape(&line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::{HigherIsBetter, LowerIsBetter};

    fn frontier_of(rows: &[[f64; 3]], dirs: &[Direction], noise: f64) -> Vec<usize> {
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        dominators(&slices, dirs, noise)
            .iter()
            .enumerate()
            .filter(|(_, (by, _))| by.is_empty())
            .map(|(i, _)| i)
            .collect()
    }

    /// time (ms), energy (J), memory (MB) of rust, c++, go, java, python
    const FIXTURE: [[f64; 3]; 5] = [
        [10.0, 1.2, 60.0],
        [8.0, 2.0, 80.0],
        [14.0, 0.9, 45.0],
        [19.0, 2.5, 140.0],
        [24.0, 3.1, 35.0],
    ];

    #[test]
    fn five_implementation_frontier() {
        // java is worse than rust in all three; python has the least memory; the rest trade off
        assert_eq!(
            frontier_of(&FIXTURE, &[LowerIsBetter; 3], 0.0),
            vec![0, 1, 2, 4]
        );
        // time and energy only: python (24 ms, 3.1 J) is dominated by everything
        let two: Vec<[f64; 3]> = FIXTURE.to_vec();
        let slices: Vec<&[f64]> = two.iter().map(|r| &r[..2]).collect();
        let d = dominators(&slices, &[LowerIsBetter; 2], 0.0);
        let frontier: Vec<usize> = (0..5).filter(|&i| d[i].0.is_empty()).collect();
        assert_eq!(frontier, vec![0, 1, 2]);
        assert_eq!(d[3].0, vec![0, 1, 2]);
    }

    #[test]
    fn higher_is_better_flips_the_axis() {
        // throughput (higher) vs energy (lower)
        let rows = [[100.0, 5.0, 0.0], [200.0, 5.0, 0.0], [150.0, 9.0, 0.0]];
        let dirs = [HigherIsBetter, LowerIsBetter, LowerIsBetter];
        assert_eq!(frontier_of(&rows, &dirs, 0.0), vec![1]);
        assert_eq!(frontier_of(&rows, &[LowerIsBetter; 3], 0.0), vec![0]);
    }

    #[test]
    fn equal_points_do_not_dominate_each_other() {
        let rows = [[1.0, 1.0, 1.0], [1.0, 1.0, 1.0]];
        assert_eq!(frontier_of(&rows, &[LowerIsBetter; 3], 0.0), vec![0, 1]);
    }

    #[test]
    fn noise_threshold_keeps_close_points_on_the_frontier() {
        // b is 2% slower and equal elsewhere: strictly dominated, not beyond 5% noise
        let rows = [[100.0, 1.0, 1.0], [102.0, 1.0, 1.0]];
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        let strict = dominators(&slices, &[LowerIsBetter; 3], 0.0);
        assert_eq!(strict[1].0, vec![0]);
        let noisy = dominators(&slices, &[LowerIsBetter; 3], 0.05);
        assert!(noisy[1].0.is_empty());
        assert_eq!(noisy[1].1, vec![0], "strict dominator is still reported");
        // 20% slower is beyond the noise
        let rows = [[100.0, 1.0, 1.0], [120.0, 1.0, 1.0]];
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        assert_eq!(dominators(&slices, &[LowerIsBetter; 3], 0.05)[1].0, vec![0]);
    }

    #[test]
    fn five_hundred_commands_are_fast() {
        let rows: Vec<[f64; 3]> = (0..500)
            .map(|i| {
                let x = i as f64;
                [x, 500.0 - x, (x * 7.0) % 97.0]
            })
            .collect();
        let slices: Vec<&[f64]> = rows.iter().map(|r| r.as_slice()).collect();
        let start = std::time::Instant::now();
        let d = dominators(&slices, &[LowerIsBetter; 3], 0.0);
        assert!(start.elapsed().as_secs() < 5);
        assert_eq!(d.len(), 500);
        // time and energy anti-correlate, so nothing is dominated on them
        assert!(d.iter().filter(|(by, _)| by.is_empty()).count() >= 90);
    }

    #[test]
    fn parses_axes_and_constraints() {
        assert_eq!(Axis::parse("wall").unwrap().name(), "wall");
        assert_eq!(Axis::parse("wall.p95").unwrap().name(), "wall.p95");
        assert_eq!(Axis::parse("memory.MAX").unwrap().name(), "memory.max");
        assert_eq!(Axis::parse("custom:ops").unwrap().name(), "custom:ops");
        assert!(Axis::parse("wall.p101").is_err());
        assert!(Axis::parse("bogus").is_err());

        let c = Constraint::parse("memory <= 256MiB").unwrap();
        assert_eq!((c.op, c.value), (Op::Le, 256.0 * 1024.0 * 1024.0));
        let c = Constraint::parse("wall.p95<20ms").unwrap();
        assert_eq!((c.op, c.axis.name()), (Op::Lt, "wall.p95".to_string()));
        assert!((c.value - 0.02).abs() < 1e-12);
        let c = Constraint::parse("energy >= 1.5").unwrap();
        assert_eq!((c.op, c.value), (Op::Ge, 1.5));
        assert_eq!(Constraint::parse("energy > 2mJ").unwrap().value, 0.002);
        assert!(Constraint::parse("wall 20ms").is_err());
        assert!(Constraint::parse("wall <= fast").is_err());
        assert!(Constraint::parse("wall <= 5MiB").is_err());
        assert!(Constraint::parse("instructions <= 5ms").is_err());
    }

    #[test]
    fn config_rules() {
        assert_eq!(ParetoConfig::parse(None, [], None, None).unwrap(), None);
        assert!(ParetoConfig::parse(None, ["wall<=1s"], None, None).is_err());
        assert!(ParetoConfig::parse(Some("wall,wall"), [], None, None).is_err());
        assert!(ParetoConfig::parse(None, [], None, Some("5")).is_err());
        let c = ParetoConfig::parse(None, [], Some("maximize:custom:ops"), None)
            .unwrap()
            .unwrap();
        assert_eq!(c.axes.len(), 1);
        assert_eq!(c.directions(), vec![HigherIsBetter]);
        let c = ParetoConfig::parse(Some("wall,energy"), [], None, Some("5%"))
            .unwrap()
            .unwrap();
        assert_eq!(c.noise_threshold, 0.05);
        assert!(ParetoConfig::parse(Some("wall"), [], None, Some("-1")).is_err());
    }

    fn result(name: &str, wall_ms: f64, energy: Option<f64>, mem_mb: u64) -> BenchmarkResult {
        let wall = wall_ms / 1000.0;
        BenchmarkResult {
            command: name.into(),
            mean: wall,
            times: Some(vec![wall * 0.9, wall, wall * 1.1]),
            energy_joules: energy.map(|e| vec![e; 3]),
            mean_energy_joules: energy,
            memory_usage_byte: Some(vec![mem_mb * 1_000_000; 3]),
            ..Default::default()
        }
    }

    fn fixture() -> Vec<BenchmarkResult> {
        FIXTURE
            .iter()
            .zip(["rust", "c++", "go", "java", "python"])
            .map(|(r, n)| result(n, r[0], Some(r[1]), r[2] as u64))
            .collect()
    }

    fn config(pareto: &str, constraints: &[&str], objective: Option<&str>) -> ParetoConfig {
        ParetoConfig::parse(Some(pareto), constraints.iter().copied(), objective, None)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn analysis_of_the_fixture() {
        let results = fixture();
        let a = analyze(&config("wall,energy,memory", &[], None), &results);
        assert_eq!(a.frontier, vec![0, 1, 2, 4]);
        assert_eq!(
            a.dominated,
            vec![Dominated {
                command_index: 3,
                by: vec![0, 1, 2]
            }]
        );
        assert!(a.excluded.is_empty());
        let a = analyze(&config("wall,energy", &[], None), &results);
        assert_eq!(a.frontier, vec![0, 1, 2]);
    }

    #[test]
    fn constraints_filter_candidates_before_dominance() {
        let results = fixture();
        // c++ (80 MB) and java (140 MB) are out; without c++, rust dominates nothing new
        let a = analyze(&config("wall,energy", &["memory <= 70MB"], None), &results);
        assert_eq!(
            a.excluded
                .iter()
                .map(|e| (e.command_index, e.reason))
                .collect::<Vec<_>>(),
            vec![(1, "constraint_violated"), (3, "constraint_violated")]
        );
        assert_eq!(a.frontier, vec![0, 2]);
        assert_eq!(
            a.dominated
                .iter()
                .map(|d| d.command_index)
                .collect::<Vec<_>>(),
            vec![4]
        );
        // a percentile constraint
        let a = analyze(&config("wall", &["wall.p95 <= 20ms"], None), &results);
        assert_eq!(
            a.excluded
                .iter()
                .map(|e| e.command_index)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn unavailable_metrics_exclude_instead_of_counting_as_zero() {
        let mut results = fixture();
        results[2].energy_joules = None;
        results[2].mean_energy_joules = None;
        let a = analyze(&config("wall,energy", &[], None), &results);
        assert_eq!(
            a.excluded,
            vec![Excluded {
                command_index: 2,
                reason: "metric_unavailable",
                detail: "energy".into()
            }]
        );
        // go would be the best at 0 J; it must not be on the frontier
        assert!(!a.frontier.contains(&2));
        assert_eq!(a.values[2][1], None);
    }

    #[test]
    fn objective_picks_the_best_candidate() {
        let results = fixture();
        let a = analyze(
            &config("wall,energy", &["memory <= 70MB"], Some("minimize:energy")),
            &results,
        );
        let o = a.objective.unwrap();
        assert_eq!((o.best, o.value), (Some(2), Some(0.9)));
        let a = analyze(&config("wall,energy", &[], Some("maximize:wall")), &results);
        assert_eq!(a.objective.unwrap().best, Some(4));
        let a = analyze(
            &config("wall", &["wall <= 1ms"], Some("minimize:wall")),
            &results,
        );
        assert_eq!(a.objective.unwrap().best, None);
    }

    #[test]
    fn within_noise_is_reported() {
        let mut results = fixture();
        // 2% slower than rust and no better anywhere
        results.push(result("rust2", 10.2, Some(1.2), 60));
        let strict = analyze(&config("wall,energy,memory", &[], None), &results);
        assert!(strict.dominated.iter().any(|d| d.command_index == 5));
        let noisy = ParetoConfig::parse(Some("wall,energy,memory"), [], None, Some("5"))
            .unwrap()
            .unwrap();
        let noisy = analyze(&noisy, &results);
        assert!(noisy.frontier.contains(&5));
        assert_eq!(noisy.within_noise, vec![5]);
        assert_eq!(noisy.noise_threshold, Some(0.05));
    }

    #[test]
    fn markdown_and_json_shapes() {
        let results = fixture();
        let cfg = config("wall,energy", &["memory <= 100MB"], Some("minimize:energy"));
        let a = analyze(&cfg, &results);
        let md = markdown(&cfg, &a, &results);
        assert!(
            md.contains("### Pareto frontier over wall↓, energy↓ with constraints memory <= 100MB"),
            "{md}"
        );
        assert!(md.contains("| Command | wall | energy | Status |"), "{md}");
        assert!(
            md.contains("| go | 14.0 ms | 0.900 J | frontier, best objective |"),
            "{md}"
        );
        assert!(
            md.contains("| java | 19.0 ms | 2.500 J | excluded: violates 'memory <= 100MB' |"),
            "{md}"
        );
        assert!(
            md.contains("| python | 24.0 ms | 3.100 J | dominated by rust, c++, go |"),
            "{md}"
        );
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["frontier"], serde_json::json!([0, 1, 2]));
        assert_eq!(
            json["metrics"][0],
            serde_json::json!({"metric": "wall", "direction": "lower_is_better"})
        );
        assert_eq!(json["excluded"][0]["reason"], "constraint_violated");
        assert!(json.get("values").is_none());
        assert!(terminal(&cfg, &a, &results).contains("Objective minimize energy: go"));
        assert!(html(&cfg, &a, &results, &|s| s.replace('<', "&lt;"))
            .contains("<tr class=\"frontier\">"));
    }
}
