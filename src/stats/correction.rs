//! Multiple-comparison correction of p-values (#321).
//!
//! With many commands or a parameter scan, one test per pair produces false
//! positives at the rate `alpha` per test. These functions adjust a family of
//! p-values so that comparing the adjusted value with `alpha` controls the
//! family-wise error rate (Bonferroni, Holm) or the false discovery rate
//! (Benjamini-Hochberg). The results equal `statsmodels.multipletests`.

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Correction {
    #[default]
    None,
    /// `min(1, m p)`; controls the family-wise error rate
    Bonferroni,
    /// Holm (1979) step-down; controls the family-wise error rate, never less
    /// powerful than Bonferroni
    Holm,
    /// Benjamini and Hochberg (1995) step-up; controls the false discovery rate
    BenjaminiHochberg,
}

impl Correction {
    pub fn is_none(&self) -> bool {
        matches!(self, Correction::None)
    }

    /// The name used on the command line and in the export.
    pub fn name(&self) -> &'static str {
        match self {
            Correction::None => "none",
            Correction::Bonferroni => "bonferroni",
            Correction::Holm => "holm",
            Correction::BenjaminiHochberg => "bh",
        }
    }
}

impl FromStr for Correction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "none" => Ok(Correction::None),
            "bonferroni" => Ok(Correction::Bonferroni),
            "holm" => Ok(Correction::Holm),
            "bh" => Ok(Correction::BenjaminiHochberg),
            other => Err(format!(
                "invalid --correction '{other}': expected 'none', 'bonferroni', 'holm' or 'bh'"
            )),
        }
    }
}

/// Which comparisons form the family that is corrected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Family {
    /// Every command against the reference
    #[default]
    Reference,
    /// Every pair of commands
    AllPairs,
}

impl Family {
    pub fn name(&self) -> &'static str {
        match self {
            Family::Reference => "reference",
            Family::AllPairs => "all-pairs",
        }
    }
}

impl FromStr for Family {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "reference" => Ok(Family::Reference),
            "all-pairs" => Ok(Family::AllPairs),
            other => Err(format!(
                "invalid --comparison-family '{other}': expected 'reference' or 'all-pairs'"
            )),
        }
    }
}

/// Adjusted p-values, in the order of `p`. Each is in [0, 1] and at least the
/// raw value. NaN input stays NaN and does not count towards the family size.
pub fn adjust(p: &[f64], method: Correction) -> Vec<f64> {
    let mut order: Vec<usize> = (0..p.len()).filter(|&i| !p[i].is_nan()).collect();
    order.sort_by(|&a, &b| p[a].total_cmp(&p[b]));
    let m = order.len() as f64;
    let mut adjusted = vec![f64::NAN; p.len()];
    match method {
        Correction::None => return p.to_vec(),
        Correction::Bonferroni => {
            for &i in &order {
                adjusted[i] = (p[i] * m).min(1.0);
            }
        }
        Correction::Holm => {
            // step-down: the running maximum keeps the order monotone
            let mut running = 0.0_f64;
            for (rank, &i) in order.iter().enumerate() {
                running = running.max((p[i] * (m - rank as f64)).min(1.0));
                adjusted[i] = running;
            }
        }
        Correction::BenjaminiHochberg => {
            // step-up: the running minimum from the largest p downwards
            let mut running = 1.0_f64;
            for (rank, &i) in order.iter().enumerate().rev() {
                running = running.min(p[i] * m / (rank as f64 + 1.0));
                adjusted[i] = running;
            }
        }
    }
    adjusted
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_relative_eq;

    fn check(p: &[f64], method: Correction, expected: &[f64]) {
        let got = adjust(p, method);
        assert_eq!(got.len(), expected.len());
        for (g, e) in got.iter().zip(expected) {
            assert_relative_eq!(g, e, max_relative = 1e-12, epsilon = 1e-15);
        }
    }

    // statsmodels 0.15.0 `multipletests(p, method=...)[1]`, SciPy 1.18.1 environment
    const P4: [f64; 4] = [0.01, 0.04, 0.03, 0.005];
    const P_TIES: [f64; 5] = [0.0, 0.02, 0.02, 0.2, 0.9];
    // A 25 p-value vector
    const P25: [f64; 25] = [
        0.001, 0.008, 0.039, 0.041, 0.042, 0.06, 0.074, 0.205, 0.212, 0.216, 0.222, 0.251, 0.269,
        0.275, 0.34, 0.341, 0.384, 0.569, 0.594, 0.696, 0.762, 0.94, 0.942, 0.975, 0.986,
    ];

    #[test]
    fn bonferroni_matches_statsmodels() {
        check(&P4, Correction::Bonferroni, &[0.04, 0.16, 0.12, 0.02]);
        check(&P_TIES, Correction::Bonferroni, &[0.0, 0.1, 0.1, 1.0, 1.0]);
        let got = adjust(&P25, Correction::Bonferroni);
        assert_relative_eq!(got[0], 0.025);
        assert_relative_eq!(got[1], 0.2);
        assert_relative_eq!(got[2], 0.975);
        assert!(got[3..].iter().all(|&v| v == 1.0));
    }

    #[test]
    fn holm_matches_statsmodels() {
        check(&P4, Correction::Holm, &[0.03, 0.06, 0.06, 0.02]);
        check(&P_TIES, Correction::Holm, &[0.0, 0.08, 0.08, 0.4, 0.9]);
        let got = adjust(&P25, Correction::Holm);
        for (g, e) in got.iter().zip([0.025, 0.192, 0.897, 0.902, 0.902, 1.0]) {
            assert_relative_eq!(*g, e, max_relative = 1e-12);
        }
    }

    #[test]
    fn benjamini_hochberg_matches_statsmodels() {
        check(
            &P4,
            Correction::BenjaminiHochberg,
            &[0.02, 0.04, 0.04, 0.02],
        );
        check(
            &P_TIES,
            Correction::BenjaminiHochberg,
            &[0.0, 0.1 / 3.0, 0.1 / 3.0, 0.25, 0.9],
        );
        let got = adjust(&P25, Correction::BenjaminiHochberg);
        let expected = [
            0.025,
            0.1,
            0.21,
            0.21,
            0.21,
            0.25,
            0.264285714285714,
            0.491071428571429,
            0.491071428571429,
            0.491071428571429,
            0.491071428571429,
            0.491071428571429,
            0.491071428571429,
            0.491071428571429,
            0.5328125,
            0.5328125,
            0.564705882352941,
            0.781578947368421,
            0.781578947368421,
            0.87,
            0.907142857142857,
            0.986,
            0.986,
            0.986,
            0.986,
        ];
        for (g, e) in got.iter().zip(expected) {
            assert_relative_eq!(*g, e, max_relative = 1e-12);
        }
    }

    #[test]
    fn a_single_comparison_is_unchanged() {
        for method in [
            Correction::Bonferroni,
            Correction::Holm,
            Correction::BenjaminiHochberg,
        ] {
            check(&[0.3], method, &[0.3]);
            check(&[0.0], method, &[0.0]);
        }
        assert!(adjust(&[], Correction::Holm).is_empty());
    }

    #[test]
    fn none_returns_the_input() {
        assert_eq!(adjust(&P4, Correction::None), P4.to_vec());
    }

    #[test]
    fn adjusted_values_are_bounded_and_ordered() {
        // adjusted >= raw, <= 1, and the adjustment keeps the order of p;
        // Holm is never below BH, Bonferroni never below Holm
        let p = [0.5, 0.001, 0.2, 0.2, 0.049, 0.9, 0.0, 0.03];
        let b = adjust(&p, Correction::Bonferroni);
        let h = adjust(&p, Correction::Holm);
        let bh = adjust(&p, Correction::BenjaminiHochberg);
        for i in 0..p.len() {
            assert!(p[i] <= bh[i] && bh[i] <= h[i] && h[i] <= b[i] && b[i] <= 1.0);
            for j in 0..p.len() {
                if p[i] <= p[j] {
                    assert!(h[i] <= h[j] && bh[i] <= bh[j] && b[i] <= b[j]);
                }
            }
        }
    }

    #[test]
    fn nan_stays_out_of_the_family() {
        let got = adjust(&[0.01, f64::NAN, 0.04], Correction::Bonferroni);
        assert_relative_eq!(got[0], 0.02);
        assert!(got[1].is_nan());
        assert_relative_eq!(got[2], 0.08);
    }

    #[test]
    fn parses_names() {
        assert_eq!("holm".parse(), Ok(Correction::Holm));
        assert_eq!("bh".parse(), Ok(Correction::BenjaminiHochberg));
        assert_eq!("bonferroni".parse(), Ok(Correction::Bonferroni));
        assert_eq!("none".parse(), Ok(Correction::None));
        assert!("fdr".parse::<Correction>().is_err());
        assert_eq!("all-pairs".parse(), Ok(Family::AllPairs));
        assert_eq!("reference".parse(), Ok(Family::Reference));
        assert!("all".parse::<Family>().is_err());
    }
}
