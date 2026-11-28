//! Student's t distribution: CDF and quantile function.
//!
//! Needed for confidence levels other than 95% (`--confidence-level`), and for
//! the equivalence test (`--equivalence-margin`). Implemented from the
//! regularized incomplete beta function (continued fraction, Lentz's method)
//! and a Lanczos approximation of ln Γ; the quantile is found by bisection.
//! Accurate to about 1e-12 relative, checked against SciPy in the tests.

/// ln Γ(x) for x > 0 (Lanczos approximation, g = 7, n = 9).
pub(crate) fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
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
    if x < 0.5 {
        // reflection
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Continued fraction of the incomplete beta function (modified Lentz).
fn beta_continued_fraction(a: f64, b: f64, x: f64) -> f64 {
    const TINY: f64 = 1e-300;
    let (qab, qap, qam) = (a + b, a + 1.0, a - 1.0);
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=500 {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;
        if (delta - 1.0).abs() < 1e-15 {
            break;
        }
    }
    h
}

/// Regularized incomplete beta function I_x(a, b).
pub fn regularized_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let front =
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_continued_fraction(a, b, x) / a
    } else {
        1.0 - front * beta_continued_fraction(b, a, 1.0 - x) / b
    }
}

/// CDF of Student's t distribution with `df` (> 0, may be fractional) degrees of freedom.
pub fn t_cdf(t: f64, df: f64) -> f64 {
    if t.is_nan() || df.is_nan() || df <= 0.0 {
        return f64::NAN;
    }
    if t.is_infinite() {
        return if t > 0.0 { 1.0 } else { 0.0 };
    }
    let tail = 0.5 * regularized_beta(df / 2.0, 0.5, df / (df + t * t));
    if t > 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

/// Quantile (inverse CDF) of Student's t distribution, for `p` in (0, 1).
pub fn t_quantile(p: f64, df: f64) -> f64 {
    if !(p > 0.0 && p < 1.0) || df.is_nan() || df <= 0.0 {
        return f64::NAN;
    }
    if p == 0.5 {
        return 0.0;
    }
    if p < 0.5 {
        return -t_quantile(1.0 - p, df);
    }
    // bracket, then bisect: the CDF is increasing
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    while t_cdf(hi, df) < p {
        lo = hi;
        hi *= 2.0;
        if hi > 1e300 {
            return f64::INFINITY;
        }
    }
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if t_cdf(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
        if (hi - lo) <= 1e-15 * hi.max(1.0) {
            break;
        }
    }
    0.5 * (lo + hi)
}

/// The two-sided critical value for `confidence` (e.g. 0.95 → t at 0.975).
pub fn t_critical(confidence: f64, df: f64) -> f64 {
    t_quantile(0.5 + confidence / 2.0, df)
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    // Reference values from SciPy 1.18.1: scipy.stats.t.ppf(p, df) and scipy.stats.t.cdf(t, df).
    #[test]
    fn quantiles_match_scipy() {
        let cases = [
            (0.975, 10.0, 2.228138851986274),
            (0.975, 1.0, 12.706204736174694),
            (0.995, 5.0, 4.032142983555227),
            (0.95, 20.0, 1.7247182429207875),
            (0.9, 3.0, 1.6377443536962093),
            (0.975, 100.0, 1.983971518523552),
            (0.999, 7.0, 4.785289628638333),
            (0.8, 50.0, 0.8488692445086644),
        ];
        for (p, df, expected) in cases {
            assert_relative_eq!(t_quantile(p, df), expected, max_relative = 1e-9);
        }
    }

    #[test]
    fn cdf_matches_scipy() {
        let cases = [
            (1.0, 3.0, 0.8044988905221148),
            (2.0, 10.0, 0.9633059826146299),
            (-1.5, 7.0, 0.088649243494985),
            (0.0, 5.0, 0.5),
            (4.0, 2.5, 0.9804935120793409),
        ];
        for (t, df, expected) in cases {
            assert_relative_eq!(t_cdf(t, df), expected, max_relative = 1e-9);
        }
    }

    #[test]
    fn quantile_is_symmetric_and_inverts_the_cdf() {
        assert_relative_eq!(t_quantile(0.025, 10.0), -t_quantile(0.975, 10.0));
        for df in [1.5, 4.0, 30.0] {
            for p in [0.6, 0.9, 0.99] {
                assert_relative_eq!(t_cdf(t_quantile(p, df), df), p, max_relative = 1e-10);
            }
        }
        assert!(t_quantile(0.0, 3.0).is_nan());
        assert!(t_quantile(0.9, 0.0).is_nan());
    }

    #[test]
    fn critical_value_of_95_percent() {
        assert_relative_eq!(
            t_critical(0.95, 10.0),
            2.228138851986274,
            max_relative = 1e-9
        );
    }
}
