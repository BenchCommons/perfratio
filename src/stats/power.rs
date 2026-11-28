//! Statistical power, minimum detectable effect and runs needed (#367,
//! `docs/specs/statistical-power.md`).
//!
//! The model is the one of `statsmodels.stats.power.TTestIndPower` with
//! `ratio = 1`: two independent samples of `n` runs with the same standard
//! deviation, compared with a two-sided equal-variance t-test at level
//! `alpha`. The power is the probability that a true difference of `d`
//! standard deviations is declared significant:
//! `1 - F(t) + F(-t)` with `F` the CDF of the noncentral t distribution with
//! `2n - 2` degrees of freedom and noncentrality `d sqrt(n / 2)`, and `t` the
//! critical value of the central t distribution.
//!
//! `n` is continuous, as in statsmodels, so that the results can be compared
//! with its reference values; [`runs_needed`] rounds up.

use super::dist::{ln_gamma, t_quantile};
use super::effect::normal_quantile;

/// CDF of the standard normal distribution (series of erf, non-alternating
/// so that it does not lose digits).
pub fn normal_cdf(x: f64) -> f64 {
    let z = x / std::f64::consts::SQRT_2;
    if z.abs() > 6.5 {
        return if x > 0.0 { 1.0 } else { 0.0 };
    }
    // erf(z) = 2/sqrt(pi) exp(-z²) Σ 2^n z^(2n+1) / (1·3·…·(2n+1))
    let z2 = z * z;
    let mut term = z;
    let mut sum = z;
    let mut k = 0.0;
    while term.abs() > 1e-17 * sum.abs() && k < 500.0 {
        k += 1.0;
        term *= 2.0 * z2 / (2.0 * k + 1.0);
        sum += term;
    }
    let erf = 2.0 / std::f64::consts::PI.sqrt() * (-z2).exp() * sum;
    0.5 * (1.0 + erf)
}

/// CDF at `t` of the noncentral t distribution with `df` degrees of freedom
/// and noncentrality `delta`:
/// `F(t) = ∫ Φ(t u - δ) g(u) du`, with `g` the density of `sqrt(χ²_df / df)`
/// (composite Simpson rule around its mode; absolute error about 1e-10).
pub fn noncentral_t_cdf(t: f64, df: f64, delta: f64) -> f64 {
    if t.is_nan() || df.is_nan() || delta.is_nan() || df <= 0.0 {
        return f64::NAN;
    }
    let sigma = 1.0 / (2.0 * df).sqrt();
    let lo = (1.0 - 14.0 * sigma).max(0.0);
    let hi = 1.0 + 14.0 * sigma;
    let ln_norm = std::f64::consts::LN_2 + 0.5 * df * df.ln()
        - 0.5 * df * std::f64::consts::LN_2
        - ln_gamma(0.5 * df);
    let density = |u: f64| {
        if u <= 0.0 {
            return if df == 1.0 { ln_norm.exp() } else { 0.0 };
        }
        (ln_norm + (df - 1.0) * u.ln() - 0.5 * df * u * u).exp()
    };
    const N: usize = 2000; // even
    let h = (hi - lo) / N as f64;
    let f = |u: f64| normal_cdf(t * u - delta) * density(u);
    let mut sum = f(lo) + f(hi);
    for i in 1..N {
        let weight = if i % 2 == 1 { 4.0 } else { 2.0 };
        sum += weight * f(lo + i as f64 * h);
    }
    (sum * h / 3.0).clamp(0.0, 1.0)
}

/// Power of the two-sided two-sample t-test at level `alpha` to detect a
/// difference of `d` standard deviations with `n` runs per sample.
/// `None` for `n < 2`, `alpha` outside (0, 1) or a non-finite `d`.
pub fn power(d: f64, n: f64, alpha: f64) -> Option<f64> {
    if !(n >= 2.0 && alpha > 0.0 && alpha < 1.0 && d.is_finite()) {
        return None;
    }
    let df = 2.0 * n - 2.0;
    let nc = d.abs() * (n / 2.0).sqrt();
    let crit = t_quantile(1.0 - alpha / 2.0, df);
    let upper = 1.0 - noncentral_t_cdf(crit, df, nc);
    let lower = noncentral_t_cdf(-crit, df, nc);
    Some((upper + lower).clamp(0.0, 1.0))
}

/// Runs per sample (continuous) of the normal approximation:
/// `2 (z₁₋α/₂ + z_power)² / d²`. A lower bound of the t-based value.
pub fn runs_normal_approximation(d: f64, alpha: f64, power: f64) -> f64 {
    let z = normal_quantile(1.0 - alpha / 2.0) + normal_quantile(power);
    2.0 * z * z / (d * d)
}

/// Runs per sample (continuous, at least 2) that give the requested `power`
/// for an effect of `d` standard deviations; `None` if `d` is 0 or not
/// finite, the arguments are out of range, or more than 1e9 runs are needed.
pub fn solve_runs(d: f64, alpha: f64, power_target: f64) -> Option<f64> {
    if !(d.is_finite() && d != 0.0 && alpha > 0.0 && alpha < 1.0) {
        return None;
    }
    if !(power_target > 0.0 && power_target < 1.0) {
        return None;
    }
    let d = d.abs();
    let reached = |n: f64| power(d, n, alpha).is_some_and(|p| p >= power_target);
    let mut lo = 2.0_f64;
    if reached(lo) {
        return Some(lo);
    }
    // the normal approximation is a lower bound: start there
    let mut hi = runs_normal_approximation(d, alpha, power_target).max(lo * 2.0);
    if hi > 1e9 {
        return None;
    }
    while !reached(hi) {
        lo = hi;
        hi *= 2.0;
        if hi > 1e9 {
            return None;
        }
    }
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if reached(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
        if hi - lo < 1e-9 * hi {
            break;
        }
    }
    Some(hi)
}

/// Runs per sample (whole) that give the requested `power` for an effect of
/// `d` standard deviations. See [`solve_runs`].
pub fn runs_needed(d: f64, alpha: f64, power_target: f64) -> Option<u64> {
    // the 1e-9 slack keeps a solution that is an integer to rounding error
    solve_runs(d, alpha, power_target).map(|n| (n - 1e-7).ceil().max(2.0) as u64)
}

/// Smallest effect, in standard deviations, that the test detects with the
/// requested `power` with `n` runs per sample. `None` for `n < 2`.
pub fn minimum_detectable_d(n: f64, alpha: f64, power_target: f64) -> Option<f64> {
    if !(n >= 2.0 && alpha > 0.0 && alpha < 1.0 && power_target > 0.0 && power_target < 1.0) {
        return None;
    }
    let reached = |d: f64| power(d, n, alpha).is_some_and(|p| p >= power_target);
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    while !reached(hi) {
        lo = hi;
        hi *= 2.0;
        if hi > 1e6 {
            return None;
        }
    }
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if reached(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
        if hi - lo < 1e-10 * hi {
            break;
        }
    }
    Some(0.5 * (lo + hi))
}

/// Whether `n` runs per sample are enough to detect `d` standard deviations
/// with the requested power. Cheaper than [`solve_runs`]: below the normal
/// approximation the answer is no without evaluating the power.
pub fn enough_runs(d: f64, n: f64, alpha: f64, power_target: f64) -> bool {
    if d == 0.0 {
        return false;
    }
    if n < runs_normal_approximation(d, alpha, power_target) {
        return false;
    }
    power(d, n, alpha).is_some_and(|p| p >= power_target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    // Reference values: statsmodels 0.15.0, `TTestIndPower().solve_power(...)`
    // with `ratio=1.0`, two-sided.
    const RUNS: [(f64, f64, f64, f64); 15] = [
        (0.2, 0.05, 0.8, 393.4056930002527),
        (0.2, 0.05, 0.9, 526.3331885760534),
        (0.2, 0.01, 0.8, 585.6092645450912),
        (0.5, 0.05, 0.8, 63.765610588911635),
        (0.5, 0.05, 0.9, 85.03128411573417),
        (0.5, 0.01, 0.8, 95.10361789235006),
        (0.8, 0.05, 0.8, 25.524571854446116),
        (0.8, 0.05, 0.9, 33.82554234416029),
        (0.8, 0.01, 0.8, 38.18831277196059),
        (1.0, 0.05, 0.8, 16.714722446954013),
        (1.0, 0.05, 0.9, 22.021088441446174),
        (1.0, 0.01, 0.8, 25.066708240006726),
        (2.0, 0.05, 0.8, 5.0899945682322505),
        (2.0, 0.05, 0.9, 6.386755099409562),
        (2.0, 0.01, 0.8, 7.6605400564019925),
    ];

    #[test]
    fn runs_match_statsmodels() {
        for (d, alpha, pw, expected) in RUNS {
            let n = solve_runs(d, alpha, pw).unwrap();
            assert_relative_eq!(n, expected, max_relative = 1e-6);
        }
    }

    #[test]
    fn minimum_detectable_effect_matches_statsmodels() {
        for (n, alpha, pw, expected) in [
            (5.0, 0.05, 0.8, 2.024440208803392),
            (5.0, 0.05, 0.9, 2.347949439839802),
            (5.0, 0.1, 0.5, 1.1369094647208484),
            (10.0, 0.05, 0.8, 1.3249473683308217),
            (10.0, 0.05, 0.9, 1.5336925040567273),
            (10.0, 0.1, 0.5, 0.7639444535721208),
            (30.0, 0.05, 0.8, 0.7356198334313556),
            (30.0, 0.05, 0.9, 0.8511699210689876),
            (30.0, 0.1, 0.5, 0.4293907043960388),
            (100.0, 0.05, 0.8, 0.3981391138964959),
            (100.0, 0.05, 0.9, 0.46065917768692105),
            (100.0, 0.1, 0.5, 0.23323619615785843),
            (500.0, 0.05, 0.8, 0.1773583482866793),
            (500.0, 0.05, 0.9, 0.20520891893356313),
            (500.0, 0.1, 0.5, 0.10402033),
        ] {
            // statsmodels stops its root finder at a power error of about 1e-6
            // (power(0.7356198, 30) = 0.7999987); ours is exact to 1e-10
            let d = minimum_detectable_d(n, alpha, pw).unwrap();
            assert_relative_eq!(d, expected, max_relative = 1e-5);
            assert_relative_eq!(power(d, n, alpha).unwrap(), pw, epsilon = 1e-9);
        }
    }

    #[test]
    fn power_matches_statsmodels() {
        for (n, d, expected) in [
            (10.0, 0.5, 0.18509565629116606),
            (20.0, 0.8, 0.6934041966153817),
            (64.0, 0.5, 0.8014595579222543),
            (5.0, 1.0, 0.2862954933805975),
            (100.0, 0.2, 0.2906458716590865),
        ] {
            assert_relative_eq!(power(d, n, 0.05).unwrap(), expected, max_relative = 1e-7);
        }
    }

    #[test]
    fn normal_cdf_matches_scipy() {
        // scipy.stats.norm.cdf
        assert_relative_eq!(normal_cdf(0.0), 0.5, epsilon = 1e-15);
        assert_relative_eq!(normal_cdf(1.0), 0.8413447460685429, max_relative = 1e-13);
        assert_relative_eq!(normal_cdf(-2.0), 0.022750131948179195, max_relative = 1e-9);
        assert_relative_eq!(normal_cdf(3.0), 0.9986501019683699, max_relative = 1e-13);
    }

    #[test]
    fn runs_needed_rounds_up_and_is_enough() {
        assert_eq!(runs_needed(0.5, 0.05, 0.8), Some(64));
        assert_eq!(runs_needed(0.2, 0.05, 0.8), Some(394));
        for d in [0.3, 0.7, 1.5] {
            let n = runs_needed(d, 0.05, 0.8).unwrap();
            assert!(power(d, n as f64, 0.05).unwrap() >= 0.8);
            assert!(power(d, n as f64 - 1.0, 0.05).unwrap() < 0.8 || n == 2);
        }
    }

    #[test]
    fn enough_runs_agrees_with_the_solution() {
        assert!(!enough_runs(0.5, 63.0, 0.05, 0.8));
        assert!(enough_runs(0.5, 64.0, 0.05, 0.8));
        assert!(!enough_runs(0.0, 1e6, 0.05, 0.8));
    }

    #[test]
    fn out_of_range_arguments_have_no_answer() {
        assert_eq!(solve_runs(0.0, 0.05, 0.8), None);
        assert_eq!(solve_runs(f64::NAN, 0.05, 0.8), None);
        assert_eq!(solve_runs(0.5, 0.05, 1.0), None);
        assert_eq!(solve_runs(1e-6, 0.05, 0.8), None);
        assert_eq!(minimum_detectable_d(1.0, 0.05, 0.8), None);
        assert_eq!(power(0.5, 1.0, 0.05), None);
    }
}

#[cfg(test)]
mod stopping_tests {
    use super::*;

    /// Seeded xorshift64* + Box-Muller: the same stream on every platform.
    struct Normal(u64);
    impl Normal {
        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let x = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
            ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn next(&mut self, mean: f64, sd: f64) -> f64 {
            let (u, v) = (self.uniform(), self.uniform());
            mean + sd * (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }
    }

    /// The stopping rule of `--target-effect` on a stream: the first run count
    /// from `min_runs` on at which `enough_runs` holds for the sd seen so far.
    fn stop(rng: &mut Normal, effect: f64, min_runs: usize) -> usize {
        let mut xs: Vec<f64> = Vec::new();
        loop {
            xs.push(rng.next(1.0, 0.1));
            let n = xs.len();
            if n < min_runs.max(2) {
                continue;
            }
            let mean = xs.iter().sum::<f64>() / n as f64;
            let sd = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt();
            if enough_runs(effect * mean / sd, n as f64, 0.05, 0.8) {
                return n;
            }
        }
    }

    #[test]
    fn stopping_run_is_deterministic_and_near_the_planned_runs() {
        // true effect 5% of the mean with sd 10% of the mean: d = 0.5, 64 runs
        let mut a = Normal(0x9E37_79B9_7F4A_7C15);
        let mut b = Normal(0x9E37_79B9_7F4A_7C15);
        assert_eq!(stop(&mut a, 0.05, 10), stop(&mut b, 0.05, 10));
        let mut rng = Normal(322_367);
        let stops: Vec<usize> = (0..300).map(|_| stop(&mut rng, 0.05, 10)).collect();
        let mean = stops.iter().sum::<usize>() as f64 / stops.len() as f64;
        // the sd estimate is noisy, so single stops vary; the average is near 64
        assert!((55.0..75.0).contains(&mean), "mean stopping run {mean}");
        assert!(stops.iter().all(|&n| n >= 10));
    }
}
