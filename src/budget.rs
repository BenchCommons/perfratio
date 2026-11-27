//! `--budget`, `--budget-file`, `--on-unavailable`: per-metric regression budgets
//! with a direction and absolute limits (#325, `docs/specs/budgets.md`).
//!
//! A budget is `<METRIC><op><VALUE>`:
//! - relative (VALUE ends in `%`): bounds the change against the `--compare` baseline;
//! - absolute (a number with an optional unit): bounds the value itself.
//!
//! `=` takes the operator from the direction of the metric (`<=` if lower is better,
//! `>=` if higher is better); `<=` and `>=` are literal.

use serde::Serialize;

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::compare::Comparison;
use crate::metric_registry::{Direction as RegistryDirection, REGISTRY};
use crate::pareto::{self, Axis, Direction, Op, Statistic};

/// What a budget measures.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// A metric of `--metric`, optionally with a statistic (`wall.p95`)
    Axis(Axis),
    /// Instances per second of a `--concurrency` batch (`batch.throughput`, higher is better)
    Throughput,
}

impl Target {
    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("throughput") {
            Ok(Target::Throughput)
        } else {
            Axis::parse(s).map(Target::Axis)
        }
    }

    pub fn name(&self) -> String {
        match self {
            Target::Axis(axis) => axis.name(),
            Target::Throughput => "throughput".into(),
        }
    }

    /// Direction from the metric descriptors (#317) for registry metrics
    pub fn direction(&self) -> Direction {
        match self {
            Target::Axis(axis) => pareto::direction(axis.metric),
            Target::Throughput => {
                let higher = REGISTRY
                    .iter()
                    .find(|spec| spec.id == "batch.throughput")
                    .is_some_and(|spec| spec.direction == RegistryDirection::HigherIsBetter);
                if higher {
                    Direction::HigherIsBetter
                } else {
                    Direction::LowerIsBetter
                }
            }
        }
    }

    /// The value of the target; `None` when the result has no data for it
    /// (never 0).
    fn value(&self, result: &BenchmarkResult) -> Option<f64> {
        match self {
            Target::Axis(axis) if axis.statistic == Statistic::Mean => {
                result.primary_center(axis.metric).filter(|v| v.is_finite())
            }
            Target::Axis(axis) => axis.value(result),
            Target::Throughput => result.throughput.filter(|v| v.is_finite()),
        }
    }

    /// Per-run samples, for the significance of a change
    fn samples(&self, result: &BenchmarkResult) -> Option<Vec<f64>> {
        match self {
            Target::Axis(axis) => result.primary_samples(axis.metric),
            Target::Throughput => None,
        }
    }

    fn format(&self, value: f64) -> String {
        match self {
            Target::Axis(axis) => pareto::format_value(axis.metric, value),
            Target::Throughput => format!("{value:.2}/s"),
        }
    }
}

/// How the limit is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// Limit on `current / baseline - 1` (0.05 = 5%)
    Relative,
    /// Limit on the value, in the base unit of the metric
    Absolute,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub target: Target,
    pub kind: Kind,
    /// `Le`: the value (or change) must be at most the limit; `Ge`: at least
    pub op: Op,
    pub limit: f64,
    /// As written, trimmed
    pub text: String,
}

impl Budget {
    pub fn parse(s: &str) -> Result<Self, String> {
        let text = s.trim().to_string();
        let err = |why: &str| format!("invalid budget '{text}': {why}");
        let pos = text
            .find(['<', '>', '='])
            .ok_or_else(|| err("expected '<METRIC><op><VALUE>' with '=', '<=' or '>='"))?;
        let rest = &text[pos..];
        let (op, len) = if rest.starts_with("<=") {
            (Some(Op::Le), 2)
        } else if rest.starts_with(">=") {
            (Some(Op::Ge), 2)
        } else if rest.starts_with('=') {
            (None, 1)
        } else {
            return Err(err("use '<=' or '>=' (strict '<' and '>' are not budgets)"));
        };
        let target = Target::parse(&text[..pos]).map_err(|e| err(&e))?;
        let value = text[pos + len..].trim();
        if value.is_empty() {
            return Err(err("missing value"));
        }
        if let Some(percent) = value.strip_suffix('%') {
            let percent = percent.trim();
            let number: f64 = percent
                .parse()
                .ok()
                .filter(|v: &f64| v.is_finite())
                .ok_or_else(|| err(&format!("'{value}' is not a percentage")))?;
            let direction = target.direction();
            let op = op.unwrap_or(match direction {
                Direction::LowerIsBetter => Op::Le,
                Direction::HigherIsBetter => Op::Ge,
            });
            // '=5%' without a sign tolerates a worsening of 5%
            let signed = percent.starts_with(['+', '-']);
            let number = if signed || op != Op::Le && op != Op::Ge {
                number
            } else {
                match direction {
                    Direction::LowerIsBetter => number,
                    Direction::HigherIsBetter => -number,
                }
            };
            if number / 100.0 <= -1.0 {
                return Err(err("a relative limit must be above -100%"));
            }
            return Ok(Budget {
                target,
                kind: Kind::Relative,
                op,
                limit: number / 100.0,
                text,
            });
        }
        let Some(op) = op else {
            return Err(err(
                "an absolute limit needs '<=' or '>=' (e.g. 'memory<=256MiB'); '=' takes a percentage",
            ));
        };
        let limit = match target {
            Target::Axis(axis) => pareto::parse_quantity(axis.metric, value),
            Target::Throughput => value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("'{value}' is not a number")),
        }
        .map_err(|e| err(&e))?;
        Ok(Budget {
            target,
            kind: Kind::Absolute,
            op,
            limit,
            text,
        })
    }

    /// `--fail-if-regressed PCT` is `--budget wall=+PCT`
    pub fn wall_regression(threshold: f64) -> Self {
        Budget {
            target: Target::Axis(Axis {
                metric: crate::metric::Metric::Wall,
                statistic: Statistic::Mean,
            }),
            kind: Kind::Relative,
            op: Op::Le,
            limit: threshold,
            text: format!("wall=+{}%", threshold * 100.0),
        }
    }

    fn op_symbol(&self) -> &'static str {
        match self.op {
            Op::Ge | Op::Gt => ">=",
            Op::Le | Op::Lt => "<=",
        }
    }

    fn holds(&self, value: f64) -> bool {
        match self.op {
            Op::Ge | Op::Gt => value >= self.limit,
            Op::Le | Op::Lt => value <= self.limit,
        }
    }
}

/// The budgets of a file: a JSON array of strings or `{"budgets": [...]}`, or a TOML
/// file with `budgets = ["wall=+5%", ...]` (an array of strings, nothing else).
pub fn parse_budget_file(path: &str, content: &str) -> Result<Vec<String>, String> {
    let is_json = path.ends_with(".json")
        || (!path.ends_with(".toml") && content.trim_start().starts_with(['[', '{']));
    if is_json {
        let value: serde_json::Value = serde_json::from_str(content)
            .map_err(|e| format!("'{path}' is not valid JSON: {e}"))?;
        let list = match &value {
            serde_json::Value::Array(_) => &value,
            serde_json::Value::Object(map) => map
                .get("budgets")
                .ok_or_else(|| format!("'{path}': expected a 'budgets' array"))?,
            _ => return Err(format!("'{path}': expected an array or a 'budgets' array")),
        };
        return list
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .map(|v| v.as_str().map(str::to_string))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| format!("'{path}': 'budgets' must be an array of strings"));
    }
    parse_toml_budgets(content).map_err(|e| format!("'{path}': {e}"))
}

/// The supported TOML subset: comments, and one `budgets = [ "...", ... ]` array of
/// basic strings that may span lines.
fn parse_toml_budgets(content: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut state = 0; // 0: before the key, 1: inside the array, 2: done
    for raw in content.lines() {
        let mut chars = raw.chars().peekable();
        let mut line = String::new();
        let mut in_string = false;
        while let Some(c) = chars.next() {
            match c {
                '"' if state != 0 => {
                    in_string = !in_string;
                    line.push(c);
                }
                '\\' if in_string => {
                    line.push(c);
                    if let Some(n) = chars.next() {
                        line.push(n);
                    }
                }
                '#' if !in_string => break,
                _ => line.push(c),
            }
        }
        let mut line = line.trim();
        if line.is_empty() {
            continue;
        }
        if state == 0 {
            let after = line
                .strip_prefix("budgets")
                .map(str::trim_start)
                .and_then(|l| l.strip_prefix('='))
                .map(str::trim_start)
                .and_then(|l| l.strip_prefix('['))
                .ok_or("expected 'budgets = [\"wall=+5%\", ...]'")?;
            line = after;
            state = 1;
        } else if state == 2 {
            return Err("unexpected content after the 'budgets' array".into());
        }
        // strings, commas and the closing bracket
        let mut rest = line.trim();
        loop {
            rest = rest.trim_start_matches([',', ' ', '\t']);
            if rest.is_empty() {
                break;
            }
            if let Some(after) = rest.strip_prefix(']') {
                if !after.trim().is_empty() {
                    return Err("unexpected content after the 'budgets' array".into());
                }
                state = 2;
                break;
            }
            let body = rest
                .strip_prefix('"')
                .ok_or("'budgets' must be an array of strings")?;
            let mut value = String::new();
            let mut escaped = false;
            let mut end = None;
            for (i, c) in body.char_indices() {
                if escaped {
                    value.push(match c {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    end = Some(i);
                    break;
                } else {
                    value.push(c);
                }
            }
            let end = end.ok_or("unterminated string")?;
            out.push(value);
            rest = &body[end + 1..];
        }
    }
    if state != 2 {
        return Err("the 'budgets' array is not closed".into());
    }
    Ok(out)
}

/// What to do with a budget that could not be evaluated
/// (`--on-unavailable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OnUnavailable {
    /// Counts as a failed budget
    Fail,
    /// Reported as a warning, exit code unaffected
    #[default]
    Warn,
    /// Left out of the terminal and Markdown reports (the JSON still lists it)
    Skip,
}

impl std::str::FromStr for OnUnavailable {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "fail" => Ok(Self::Fail),
            "warn" => Ok(Self::Warn),
            "skip" => Ok(Self::Skip),
            other => Err(format!(
                "invalid '--on-unavailable' value '{other}': expected fail, warn or skip"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    /// The limit is exceeded, but the difference is not significant
    WithinNoise,
    /// No data, or too few runs, to decide; see `reason`
    NotEvaluated,
}

/// One budget applied to one command.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Evaluation {
    pub command: String,
    pub metric: String,
    pub kind: &'static str,
    /// `<=` or `>=`
    pub op: &'static str,
    /// Fraction (0.05 = +5%) for `relative`, base unit (s, B, J, count) for `absolute`
    pub limit: f64,
    /// The budget as written
    pub budget: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<f64>,
    /// `current / baseline - 1`, relative budgets only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_value: Option<f64>,
    pub verdict: Verdict,
    /// Why a budget is `not_evaluated`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip)]
    target: Option<Target>,
}

impl Evaluation {
    fn base(command: &str, budget: &Budget) -> Self {
        Evaluation {
            command: command.to_string(),
            metric: budget.target.name(),
            kind: match budget.kind {
                Kind::Relative => "relative",
                Kind::Absolute => "absolute",
            },
            op: budget.op_symbol(),
            limit: budget.limit,
            budget: budget.text.clone(),
            baseline: None,
            current: None,
            change: None,
            p_value: None,
            verdict: Verdict::NotEvaluated,
            reason: None,
            target: Some(budget.target),
        }
    }

    fn not_evaluated(mut self, reason: &'static str) -> Self {
        self.verdict = Verdict::NotEvaluated;
        self.reason = Some(reason);
        self
    }

    fn format(&self, value: f64) -> String {
        match self.target {
            Some(target) => target.format(value),
            None => format!("{value}"),
        }
    }
}

fn relative(
    budget: &Budget,
    command: &str,
    baseline: &BenchmarkResult,
    current: &BenchmarkResult,
) -> Evaluation {
    let mut e = Evaluation::base(command, budget);
    let target = budget.target;
    e.baseline = target.value(baseline);
    e.current = target.value(current);
    let (Some(b), Some(c)) = (e.baseline, e.current) else {
        let reason = if e.baseline.is_none() {
            "unavailable_in_baseline"
        } else {
            "unavailable_in_current"
        };
        return e.not_evaluated(reason);
    };
    if b <= 0.0 {
        return e.not_evaluated("baseline_not_positive");
    }
    let change = c / b - 1.0;
    e.change = Some(change);
    if budget.holds(change) {
        e.verdict = Verdict::Pass;
        return e;
    }
    // Over the limit: only a significant difference fails
    let samples = (target.samples(baseline), target.samples(current));
    match samples {
        (Some(a), Some(b)) => {
            let wall =
                matches!(target, Target::Axis(axis) if axis.metric == crate::metric::Metric::Wall);
            let p = if wall {
                crate::compare::p_value(baseline, current)
            } else {
                crate::compare::samples_p_value(&a, &b)
            };
            e.p_value = p;
            match p {
                Some(p) if p < crate::stats::settings::current().significance_level => {
                    // #323: a change within the noise threshold or shown equivalent is no failure
                    let tolerated = wall
                        && crate::stats::effect::evaluate(
                            baseline,
                            current,
                            p,
                            &crate::stats::settings::current(),
                        )
                        .is_some_and(|v| {
                            use crate::stats::effect::Classification;
                            matches!(
                                v.classification,
                                Classification::WithinNoise | Classification::Equivalent
                            )
                        });
                    e.verdict = if tolerated {
                        Verdict::WithinNoise
                    } else {
                        Verdict::Fail
                    }
                }
                Some(_) => e.verdict = Verdict::WithinNoise,
                None => return e.not_evaluated("too_few_runs"),
            }
        }
        // no per-run data (e.g. throughput): the point estimate decides
        _ => e.verdict = Verdict::Fail,
    }
    e
}

fn absolute(budget: &Budget, result: &BenchmarkResult) -> Evaluation {
    let mut e = Evaluation::base(&result.command, budget);
    e.current = budget.target.value(result);
    let Some(value) = e.current else {
        return e.not_evaluated("unavailable_in_current");
    };
    e.verdict = if budget.holds(value) {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    e
}

/// The outcome of all budgets of a run.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Report {
    /// Path of the `--compare` baseline
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<String>,
    pub on_unavailable: OnUnavailable,
    /// `pass`, `fail` or `not_evaluated` (nothing could be evaluated)
    pub verdict: &'static str,
    pub budgets: Vec<Evaluation>,
}

impl Report {
    /// Evaluations that fail the gate: failed budgets, plus the ones that could not be
    /// evaluated with `--on-unavailable fail`.
    pub fn failures(&self) -> usize {
        self.budgets
            .iter()
            .filter(|e| {
                e.verdict == Verdict::Fail
                    || (e.verdict == Verdict::NotEvaluated
                        && self.on_unavailable == OnUnavailable::Fail)
            })
            .count()
    }

    fn shown(&self) -> impl Iterator<Item = &Evaluation> {
        let skip = self.on_unavailable == OnUnavailable::Skip;
        self.budgets
            .iter()
            .filter(move |e| !(skip && e.verdict == Verdict::NotEvaluated))
    }

    fn cells(e: &Evaluation) -> [String; 7] {
        let change = e
            .change
            .map(|c| format!("{:+.1}%", c * 100.0))
            .unwrap_or_default();
        let limit = match e.kind {
            "relative" => format!("{} {:+.1}%", e.op, e.limit * 100.0),
            _ => format!("{} {}", e.op, e.format(e.limit)),
        };
        let verdict = match e.verdict {
            Verdict::Pass => "pass".to_string(),
            Verdict::Fail => "FAIL".to_string(),
            Verdict::WithinNoise => "within noise".to_string(),
            Verdict::NotEvaluated => format!("not evaluated ({})", e.reason.unwrap_or("?")),
        };
        [
            crate::util::sanitize::escape_control_chars(&e.command).into_owned(),
            e.metric.clone(),
            e.baseline.map(|v| e.format(v)).unwrap_or_default(),
            e.current.map(|v| e.format(v)).unwrap_or_default(),
            change,
            limit,
            verdict,
        ]
    }

    /// The budgets for the terminal; empty if there is nothing to show.
    pub fn terminal(&self) -> String {
        const HEADER: [&str; 7] = [
            "Command", "Metric", "Baseline", "Current", "Change", "Budget", "Verdict",
        ];
        let rows: Vec<[String; 7]> = self.shown().map(Self::cells).collect();
        if rows.is_empty() {
            return String::new();
        }
        let mut widths = HEADER.map(|h| h.chars().count());
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let line = |cells: &[String; 7]| {
            let mut out = String::new();
            for (i, (cell, width)) in cells.iter().zip(widths).enumerate() {
                let pad = width - cell.chars().count();
                if (2..=4).contains(&i) {
                    out.push_str(&format!("  {}{cell}", " ".repeat(pad)));
                } else {
                    out.push_str(&format!("  {cell}{}", " ".repeat(pad)));
                }
            }
            out.trim_end().to_string()
        };
        let mut out = format!("Budgets: {}\n", self.verdict);
        out.push_str(&line(&HEADER.map(String::from)));
        out.push('\n');
        for row in &rows {
            out.push_str(&line(row));
            out.push('\n');
        }
        for e in self.shown().filter(|e| e.verdict == Verdict::NotEvaluated) {
            let how = match self.on_unavailable {
                OnUnavailable::Fail => "counts as a failure ('--on-unavailable fail')",
                _ => "not a pass ('--on-unavailable warn')",
            };
            out.push_str(&format!(
                "Warning: budget '{}' of '{}' was not evaluated ({}): {how}\n",
                e.budget,
                crate::util::sanitize::escape_control_chars(&e.command),
                e.reason.unwrap_or("?")
            ));
        }
        out
    }

    /// The budgets as a Markdown table (appended to `--export-diff-markdown`).
    pub fn markdown(&self) -> String {
        let escape = |cell: &str| cell.replace('|', "\\|");
        let rows: Vec<[String; 7]> = self.shown().map(Self::cells).collect();
        if rows.is_empty() {
            return String::new();
        }
        let mut out = format!("\n**Budgets: {}**\n\n", self.verdict);
        out.push_str("| Command | Metric | Baseline | Current | Change | Budget | Verdict |\n");
        out.push_str("|:---|:---|---:|---:|---:|:---|:---|\n");
        for [name, rest @ ..] in rows {
            out.push_str(&format!("| `{}` |", escape(&name)));
            for cell in rest {
                let cell = if cell == "FAIL" {
                    "**FAIL**".to_string()
                } else {
                    escape(&cell)
                };
                out.push_str(&format!(" {cell} |"));
            }
            out.push('\n');
        }
        out
    }
}

/// Evaluate `budgets`: relative ones for every compared command, absolute ones for
/// every current result.
pub fn evaluate(
    budgets: &[Budget],
    comparison: Option<&Comparison<'_>>,
    current: &[BenchmarkResult],
    baseline_path: Option<&str>,
    on_unavailable: OnUnavailable,
) -> Report {
    let mut out = Vec::new();
    for budget in budgets {
        match budget.kind {
            Kind::Relative => {
                for delta in comparison.iter().flat_map(|c| c.deltas.iter()) {
                    out.push(relative(budget, delta.name, delta.baseline, delta.current));
                }
            }
            Kind::Absolute => {
                for result in current {
                    out.push(absolute(budget, result));
                }
            }
        }
    }
    let evaluated = out.iter().any(|e| e.verdict != Verdict::NotEvaluated);
    let mut report = Report {
        baseline: baseline_path.map(str::to_string),
        on_unavailable,
        verdict: "pass",
        budgets: out,
    };
    report.verdict = if report.failures() > 0 {
        "fail"
    } else if !evaluated {
        "not_evaluated"
    } else {
        "pass"
    };
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::Metric;

    fn budget(s: &str) -> Budget {
        Budget::parse(s).unwrap_or_else(|e| panic!("{e}"))
    }

    fn result(command: &str, times: &[f64]) -> BenchmarkResult {
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        BenchmarkResult {
            command: command.to_string(),
            mean,
            stddev: Some(0.0),
            times: Some(times.to_vec()),
            ..Default::default()
        }
    }

    #[test]
    fn parses_relative_and_absolute() {
        let b = budget("wall=+5%");
        assert_eq!((b.kind, b.op, b.limit), (Kind::Relative, Op::Le, 0.05));
        let b = budget("wall<=+3%");
        assert_eq!((b.kind, b.op, b.limit), (Kind::Relative, Op::Le, 0.03));
        // unsigned: a tolerated worsening
        assert_eq!(budget("energy=3%").limit, 0.03);
        assert_eq!(budget("wall = -10%").limit, -0.10);
        let b = budget("memory<=256MiB");
        assert_eq!(
            (b.kind, b.op, b.limit),
            (Kind::Absolute, Op::Le, 268435456.0)
        );
        let b = budget("wall<=50ms");
        assert_eq!(b.limit, 0.05);
        assert_eq!(budget("energy<=3J").limit, 3.0);
        assert_eq!(budget("instructions=+1%").limit, 0.01);
        let b = budget("wall.p95<=20ms");
        assert_eq!(b.limit, 0.02);
    }

    #[test]
    fn higher_is_better_is_a_lower_bound() {
        let b = budget("throughput=-2%");
        assert_eq!((b.op, b.limit), (Op::Ge, -0.02));
        // unsigned: the tolerated drop
        let b = budget("throughput=2%");
        assert_eq!((b.op, b.limit), (Op::Ge, -0.02));
        let b = budget("throughput>=100");
        assert_eq!((b.kind, b.op, b.limit), (Kind::Absolute, Op::Ge, 100.0));
    }

    #[test]
    fn rejects_bad_budgets() {
        for bad in [
            "wall",
            "wall=",
            "wall=5",
            "wall<5%",
            "wall>5%",
            "nothing=5%",
            "memory<=5ms",
            "wall=abc%",
            "wall=-100%",
            "<=5%",
            "throughput<=5ms",
        ] {
            assert!(Budget::parse(bad).is_err(), "{bad}");
        }
    }

    // metric, limit text, baseline mean, current mean (no per-run data) -> verdict
    #[test]
    fn table_of_relative_verdicts_by_direction() {
        let table = [
            ("throughput=-2%", 100.0, 99.0, Verdict::Pass),
            ("throughput=-2%", 100.0, 97.0, Verdict::Fail),
            ("throughput=-2%", 100.0, 150.0, Verdict::Pass),
            ("throughput=+10%", 100.0, 105.0, Verdict::Fail),
            ("throughput=+10%", 100.0, 120.0, Verdict::Pass),
        ];
        for (text, base, cur, expected) in table {
            let mk = |t: f64| BenchmarkResult {
                command: "c".into(),
                throughput: Some(t),
                ..Default::default()
            };
            let (b, c) = (mk(base), mk(cur));
            assert_eq!(
                relative(&budget(text), "c", &b, &c).verdict,
                expected,
                "{text} {base} {cur}"
            );
        }
    }

    #[test]
    fn lower_is_better_relative_with_noise() {
        let base = result("a", &[1.0, 1.01, 0.99, 1.0, 1.0]);
        let slow = result("a", &[1.2, 1.21, 1.19, 1.2, 1.2]);
        let ok = result("a", &[1.02, 1.03, 1.01, 1.02, 1.02]);
        assert_eq!(
            relative(&budget("wall=+5%"), "a", &base, &slow).verdict,
            Verdict::Fail
        );
        assert_eq!(
            relative(&budget("wall=+5%"), "a", &base, &ok).verdict,
            Verdict::Pass
        );
        // over the limit (+10%), but within noise
        let over = result("a", &[1.5, 0.7, 1.4, 0.8, 1.2]);
        assert_eq!(
            relative(&budget("wall=+5%"), "a", &base, &over).verdict,
            Verdict::WithinNoise
        );
        // faster is never over a '+' budget; 'wall=-10%' demands an improvement
        let fast = result("a", &[0.5, 0.51, 0.49, 0.5, 0.5]);
        assert_eq!(
            relative(&budget("wall=+5%"), "a", &base, &fast).verdict,
            Verdict::Pass
        );
        assert_eq!(
            relative(&budget("wall=-10%"), "a", &base, &fast).verdict,
            Verdict::Pass
        );
        assert_eq!(
            relative(&budget("wall=-10%"), "a", &base, &slow).verdict,
            Verdict::Fail
        );
    }

    #[test]
    fn too_few_runs_is_not_evaluated() {
        let base = result("a", &[1.0, 1.0]);
        let slow = result("a", &[2.0, 2.0]);
        let e = relative(&budget("wall=+5%"), "a", &base, &slow);
        assert_eq!(
            (e.verdict, e.reason),
            (Verdict::NotEvaluated, Some("too_few_runs"))
        );
    }

    #[test]
    fn absolute_budgets() {
        let mut r = result("a", &[0.04, 0.04, 0.04]);
        assert_eq!(absolute(&budget("wall<=50ms"), &r).verdict, Verdict::Pass);
        assert_eq!(absolute(&budget("wall<=30ms"), &r).verdict, Verdict::Fail);
        assert_eq!(absolute(&budget("wall>=30ms"), &r).verdict, Verdict::Pass);
        r.memory_usage_byte = Some(vec![100 << 20, 100 << 20, 100 << 20]);
        assert_eq!(
            absolute(&budget("memory<=256MiB"), &r).verdict,
            Verdict::Pass
        );
        assert_eq!(
            absolute(&budget("memory<=64MiB"), &r).verdict,
            Verdict::Fail
        );
    }

    #[test]
    fn unavailable_follows_the_policy_and_is_never_zero() {
        let base = result("a", &[1.0; 5]);
        let cur = result("a", &[1.0; 5]);
        // no energy data on either side
        let comparison =
            crate::compare::compare(std::slice::from_ref(&base), std::slice::from_ref(&cur));
        let budgets = [budget("energy=+3%"), budget("energy<=3J")];
        let results = [cur.clone()];
        for (policy, failures, verdict) in [
            (OnUnavailable::Fail, 2, "fail"),
            (OnUnavailable::Warn, 0, "not_evaluated"),
            (OnUnavailable::Skip, 0, "not_evaluated"),
        ] {
            let report = evaluate(
                &budgets,
                Some(&comparison),
                &results,
                Some("b.json"),
                policy,
            );
            assert_eq!(report.failures(), failures, "{policy:?}");
            assert_eq!(report.verdict, verdict, "{policy:?}");
            assert!(report
                .budgets
                .iter()
                .all(|e| e.current.is_none() && e.change.is_none()));
            assert_eq!(report.budgets.len(), 2);
            let shown = report.terminal().contains("energy");
            assert_eq!(shown, policy != OnUnavailable::Skip, "{policy:?}");
        }
    }

    #[test]
    fn several_budgets_one_failure_fails_the_report() {
        let base = result("a", &[1.0, 1.01, 0.99, 1.0, 1.0]);
        let cur = result("a", &[1.2, 1.21, 1.19, 1.2, 1.2]);
        let comparison =
            crate::compare::compare(std::slice::from_ref(&base), std::slice::from_ref(&cur));
        let budgets = [budget("wall=+50%"), budget("wall<=1ms"), budget("wall=+5%")];
        let report = evaluate(
            &budgets,
            Some(&comparison),
            std::slice::from_ref(&cur),
            None,
            OnUnavailable::Warn,
        );
        let verdicts: Vec<_> = report.budgets.iter().map(|e| e.verdict).collect();
        assert_eq!(verdicts, [Verdict::Pass, Verdict::Fail, Verdict::Fail]);
        assert_eq!((report.failures(), report.verdict), (2, "fail"));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["budgets"][1]["kind"], "absolute");
        assert_eq!(json["budgets"][2]["verdict"], "fail");
        assert!(json["budgets"][2]["change"].as_f64().unwrap() > 0.19);
    }

    #[test]
    fn budget_files() {
        let json = r#"{"budgets": ["wall=+5%", "memory<=256MiB"]}"#;
        assert_eq!(parse_budget_file("b.json", json).unwrap().len(), 2);
        assert_eq!(
            parse_budget_file("b.json", r#"["wall=+5%"]"#).unwrap(),
            ["wall=+5%"]
        );
        let toml = "# gates\nbudgets = [\n  \"wall=+5%\", # time\n  \"memory<=256MiB\",\n]\n";
        assert_eq!(
            parse_budget_file("b.toml", toml).unwrap(),
            ["wall=+5%", "memory<=256MiB"]
        );
        assert_eq!(
            parse_budget_file("b.toml", r#"budgets = ["wall=+5%"]"#).unwrap(),
            ["wall=+5%"]
        );
        for bad in [
            "wall = 5",
            "budgets = [\"a\"",
            "budgets = [1]",
            "budgets = [\"a\"] x",
            "[x]\nbudgets = []",
        ] {
            assert!(parse_budget_file("b.toml", bad).is_err(), "{bad}");
        }
        assert!(parse_budget_file("b.json", "{}").is_err());
        assert!(Metric::Wall.is_time());
    }
}
