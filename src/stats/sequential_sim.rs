//! Coverage study of the stopping rules of `--target-precision` (#322).
//!
//! Streams of independent (and one autocorrelated) run times with a known mean are
//! generated from a seed. Both rules see the same stream. After every run from
//! `min_runs` on, a rule stops when the relative half-width of its interval is at most
//! the target. Two error rates are measured per rule:
//!
//! - `stop_miss`: the share of streams whose interval at the stopping run misses the
//!   true mean (what a user of `--target-precision` experiences);
//! - `ever_miss`: the share of streams in which the interval misses the true mean at
//!   *any* run between `min_runs` and `cap` (the time-uniform error that a confidence
//!   sequence bounds by alpha).
//!
//! `coverage_study` (ignored, run with `--ignored --nocapture`, ideally `--release`)
//! prints the table of `docs/specs/sequential-stopping.md`.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::stats::precision::t_critical;
use crate::stats::sequential::{half_width, rho_squared, TUNED_AT_RUNS};

#[derive(Debug, Clone, Copy)]
pub enum Dist {
    /// Normal(1, 0.3²)
    Normal,
    /// exp(Normal(0, 0.5²)): right-skewed, CV 0.53
    LogNormal,
    /// 80% Normal(1, 0.05²), 20% Normal(2, 0.1²): a fast and a slow mode
    Bimodal,
    /// AR(1) with coefficient 0.5 and stationary Normal(1, 0.3²) margins
    Autocorrelated,
    /// exp(Normal(0, 1)): heavy right tail, CV 1.3
    HeavyLogNormal,
}

impl Dist {
    pub const ALL: [Dist; 5] = [
        Dist::Normal,
        Dist::LogNormal,
        Dist::Bimodal,
        Dist::Autocorrelated,
        Dist::HeavyLogNormal,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Dist::Normal => "normal",
            Dist::LogNormal => "log-normal (sigma 0.5)",
            Dist::Bimodal => "bimodal",
            Dist::Autocorrelated => "autocorrelated (phi 0.5)",
            Dist::HeavyLogNormal => "log-normal (sigma 1)",
        }
    }

    pub fn mean(self) -> f64 {
        match self {
            Dist::Normal | Dist::Autocorrelated => 1.0,
            Dist::LogNormal => (0.5f64 * 0.5 / 2.0).exp(),
            Dist::Bimodal => 0.8 * 1.0 + 0.2 * 2.0,
            Dist::HeavyLogNormal => 0.5f64.exp(),
        }
    }
}

fn normal(rng: &mut StdRng) -> f64 {
    // Box-Muller
    let u1: f64 = 1.0 - rng.gen::<f64>();
    let u2: f64 = rng.gen();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

pub struct Stream {
    dist: Dist,
    rng: StdRng,
    previous: f64,
}

impl Stream {
    pub fn new(dist: Dist, seed: u64) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let previous = 0.3 * normal(&mut rng);
        Stream {
            dist,
            rng,
            previous,
        }
    }

    pub fn next(&mut self) -> f64 {
        let r = &mut self.rng;
        match self.dist {
            Dist::Normal => 1.0 + 0.3 * normal(r),
            Dist::LogNormal => (0.5 * normal(r)).exp(),
            Dist::HeavyLogNormal => normal(r).exp(),
            Dist::Bimodal => {
                if r.gen::<f64>() < 0.8 {
                    1.0 + 0.05 * normal(r)
                } else {
                    2.0 + 0.1 * normal(r)
                }
            }
            Dist::Autocorrelated => {
                let phi = 0.5;
                self.previous = phi * self.previous + (1.0 - phi * phi).sqrt() * 0.3 * normal(r);
                1.0 + self.previous
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RuleOutcome {
    /// Streams that reached the target before the cap
    pub stopped: u32,
    /// Of those, the interval at the stopping run missed the true mean
    pub stop_miss: u32,
    /// The interval missed the true mean at some run in `min_runs..=cap`
    pub ever_miss: u32,
    /// Sum of the stopping run counts of the streams that stopped
    pub runs: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Study {
    pub fixed: RuleOutcome,
    pub sequential: RuleOutcome,
}

impl RuleOutcome {
    pub fn stop_miss_rate(&self) -> f64 {
        self.stop_miss as f64 / self.stopped.max(1) as f64
    }
    pub fn mean_runs(&self) -> f64 {
        self.runs as f64 / self.stopped.max(1) as f64
    }
}

/// Run `replicates` streams of `dist` through both rules.
pub fn study(
    dist: Dist,
    min_runs: usize,
    target: f64,
    cap: usize,
    replicates: u32,
    seed: u64,
    confidence: f64,
    tuned_at: f64,
) -> Study {
    let alpha = 1.0 - confidence;
    let rho2 = rho_squared(alpha, tuned_at);
    let truth = dist.mean();
    let mut out = Study::default();
    for replicate in 0..replicates {
        let mut stream = Stream::new(dist, seed.wrapping_add(replicate as u64 * 0x9E37_79B9));
        let (mut n, mut mean, mut m2) = (0usize, 0.0f64, 0.0f64);
        let mut stopped = [false; 2];
        let mut ever = [false; 2];
        for _ in 0..cap {
            let x = stream.next();
            n += 1;
            let delta = x - mean;
            mean += delta / n as f64;
            m2 += delta * (x - mean);
            if n < min_runs.max(2) {
                continue;
            }
            let sd = (m2 / (n - 1) as f64).sqrt();
            let widths = [
                t_critical(confidence, n - 1) * sd / (n as f64).sqrt(),
                half_width(n, sd, alpha, rho2),
            ];
            for (i, width) in widths.into_iter().enumerate() {
                let covers = (mean - truth).abs() <= width;
                if !covers {
                    ever[i] = true;
                }
                if !stopped[i] && mean > 0.0 && width / mean <= target {
                    stopped[i] = true;
                    let outcome = if i == 0 {
                        &mut out.fixed
                    } else {
                        &mut out.sequential
                    };
                    outcome.stopped += 1;
                    outcome.runs += n as u64;
                    if !covers {
                        outcome.stop_miss += 1;
                    }
                }
            }
        }
        for (i, &e) in ever.iter().enumerate() {
            let outcome = if i == 0 {
                &mut out.fixed
            } else {
                &mut out.sequential
            };
            if e {
                outcome.ever_miss += 1;
            }
        }
    }
    out
}

fn se(p: f64, n: u32) -> f64 {
    (p * (1.0 - p) / n.max(1) as f64).sqrt()
}

#[test]
#[ignore = "long study: cargo test --release sequential_sim::coverage_study -- --ignored --nocapture"]
fn coverage_study() {
    let replicates: u32 = std::env::var("PERFRATIO_SIM_REPLICATES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4000);
    println!("replicates = {replicates}, confidence 95%, target ±10%, cap 3000 runs, seed 322");
    println!(
        "{:<26}{:>5} | {:>8} {:>8} {:>7} | {:>8} {:>8} {:>7} | {:>6}",
        "distribution",
        "min",
        "fix stop",
        "fix ever",
        "fix n",
        "seq stop",
        "seq ever",
        "seq n",
        "cost"
    );
    for dist in Dist::ALL {
        for min_runs in [3usize, 5, 10, 30] {
            let s = study(
                dist,
                min_runs,
                0.10,
                3000,
                replicates,
                322,
                0.95,
                TUNED_AT_RUNS,
            );
            println!(
                "{:<26}{:>5} | {:>7.1}% {:>7.1}% {:>7.1} | {:>7.1}% {:>7.1}% {:>7.1} | {:>5.2}x",
                dist.name(),
                min_runs,
                100.0 * s.fixed.stop_miss_rate(),
                100.0 * s.fixed.ever_miss as f64 / replicates as f64,
                s.fixed.mean_runs(),
                100.0 * s.sequential.stop_miss_rate(),
                100.0 * s.sequential.ever_miss as f64 / replicates as f64,
                s.sequential.mean_runs(),
                s.sequential.mean_runs() / s.fixed.mean_runs(),
            );
        }
    }
}

#[test]
#[ignore = "long study of the tuning run count"]
fn tuning_study() {
    println!("min 10, target ±10%, cap 3000, 2000 replicates: cost of the tuning run count");
    for dist in [Dist::Normal, Dist::LogNormal, Dist::HeavyLogNormal] {
        for tuned_at in [10.0, 30.0, 100.0, 300.0] {
            let s = study(dist, 10, 0.10, 3000, 2000, 322, 0.95, tuned_at);
            println!(
                "{:<26} tuned at {:>4}: stop miss {:>5.1}%  ever miss {:>5.1}%  runs {:>7.1} (fixed {:>6.1})",
                dist.name(), tuned_at as u32,
                100.0 * s.sequential.stop_miss_rate(),
                100.0 * s.sequential.ever_miss as f64 / 2000.0,
                s.sequential.mean_runs(), s.fixed.mean_runs()
            );
        }
    }
}

#[test]
fn sequential_keeps_the_nominal_coverage_where_its_assumptions_hold() {
    // Modest, seeded. The fixed rule is not asserted here (its miss rate is the finding of
    // the study); the sequential rule must not exceed alpha at the stopping run by more
    // than three standard errors. Multimodal data needs enough runs to see every mode:
    // 30 here (the study lists 3 to 30).
    let replicates = 400;
    let allowed = 0.05 + 3.0 * se(0.05, replicates);
    for (dist, min_runs) in [
        (Dist::Normal, 10usize),
        (Dist::Normal, 30),
        (Dist::LogNormal, 10),
        (Dist::LogNormal, 30),
        (Dist::Bimodal, 30),
    ] {
        let s = study(
            dist,
            min_runs,
            0.10,
            1500,
            replicates,
            322,
            0.95,
            TUNED_AT_RUNS,
        );
        assert!(
            s.sequential.stop_miss_rate() <= allowed,
            "{dist:?} min {min_runs}: stop miss {} > {allowed}",
            s.sequential.stop_miss_rate()
        );
        assert!(s.sequential.stopped > replicates * 9 / 10, "{dist:?}");
    }
    // The error over *all* run counts is bounded by alpha when the data are normal
    // (the asymptotic sequence is approximate for skewed data and few runs)
    let s = study(
        Dist::Normal,
        30,
        0.10,
        1500,
        replicates,
        322,
        0.95,
        TUNED_AT_RUNS,
    );
    assert!(s.sequential.ever_miss as f64 / replicates as f64 <= allowed);
}

#[test]
fn the_fixed_rule_loses_coverage_when_peeking_with_few_runs() {
    // Normal runs, a target reached after a few dozen runs, checked from run 3:
    // the first narrow interval is biased towards a small sample deviation.
    let s = study(Dist::Normal, 3, 0.10, 1500, 600, 322, 0.95, TUNED_AT_RUNS);
    assert!(
        s.fixed.stop_miss_rate() > 0.05 + 2.0 * se(0.05, 600),
        "{}",
        s.fixed.stop_miss_rate()
    );
    // ... and misses the true mean at some check far more often than alpha
    assert!(s.fixed.ever_miss as f64 / 600.0 > 0.2);
    // The sequence costs more runs
    assert!(s.sequential.mean_runs() > s.fixed.mean_runs());
}

#[test]
fn studies_are_reproducible() {
    let a = study(Dist::LogNormal, 10, 0.1, 500, 50, 7, 0.95, TUNED_AT_RUNS);
    let b = study(Dist::LogNormal, 10, 0.1, 500, 50, 7, 0.95, TUNED_AT_RUNS);
    assert_eq!(
        (a.fixed.stop_miss, a.sequential.runs, a.sequential.ever_miss),
        (b.fixed.stop_miss, b.sequential.runs, b.sequential.ever_miss)
    );
}
