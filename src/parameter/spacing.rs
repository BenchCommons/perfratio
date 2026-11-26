//! `--parameter-step-n` and `--parameter-step-log`: a fixed number of evenly or
//! geometrically spaced values of a `-P` range (#327, `docs/specs/scripting-ergonomics.md`).

use std::str::FromStr;

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::error::ParameterScanError;
use crate::util::number::Number;

/// The largest number of steps, as for `-D`
const MAX_STEPS: usize = 100_000;

/// Values of a scan and a note for the user if they were rounded or de-duplicated.
#[derive(Debug, PartialEq)]
pub struct Spaced {
    pub values: Vec<Number>,
    pub note: Option<String>,
}

fn invalid(message: impl Into<String>) -> ParameterScanError {
    ParameterScanError::InvalidSpacing(message.into())
}

/// `n + 1` values from `min` to `max`, both included: evenly spaced, or with a constant
/// ratio between neighbours if `log`.
///
/// Integer bounds give integers (rounded to nearest, duplicates removed). Other bounds give
/// decimals: linear values are rounded to 12 decimal places, geometric values to 6
/// significant digits.
pub fn spaced(min: &str, max: &str, n: usize, log: bool) -> Result<Spaced, ParameterScanError> {
    if n == 0 || n > MAX_STEPS {
        return Err(invalid(format!(
            "'--parameter-step-n' must be between 1 and {MAX_STEPS}"
        )));
    }
    if let (Ok(lo), Ok(hi)) = (min.parse::<i32>(), max.parse::<i32>()) {
        if hi < lo {
            return Err(ParameterScanError::EmptyRange);
        }
        if log && lo <= 0 {
            return Err(log_minimum(min));
        }
        return Ok(integers(lo, hi, n, log));
    }
    let lo = Decimal::from_str(min)?;
    let hi = Decimal::from_str(max)?;
    if hi < lo {
        return Err(ParameterScanError::EmptyRange);
    }
    if log && lo <= Decimal::ZERO {
        return Err(log_minimum(min));
    }
    Ok(decimals(lo, hi, n, log))
}

fn log_minimum(min: &str) -> ParameterScanError {
    invalid(format!("'--parameter-step-log' needs MIN > 0, got '{min}'"))
}

fn integers(lo: i32, hi: i32, n: usize, log: bool) -> Spaced {
    let mut values: Vec<i32> = Vec::with_capacity(n + 1);
    let mut rounded = false;
    for i in 0..=n {
        let exact: f64 = if i == 0 {
            lo.into()
        } else if i == n {
            hi.into()
        } else if log {
            f64::from(lo) * (f64::from(hi) / f64::from(lo)).powf(i as f64 / n as f64)
        } else {
            f64::from(lo) + (f64::from(hi) - f64::from(lo)) * i as f64 / n as f64
        };
        let value = exact.round();
        if (exact - value).abs() > 1e-9 * exact.abs().max(1.0) {
            rounded = true;
        }
        values.push(value as i32);
    }
    let total = values.len();
    values.dedup();
    note(rounded, total - values.len(), "integers").map_or_else(
        || Spaced {
            values: values.iter().map(|&v| Number::Int(v)).collect(),
            note: None,
        },
        |note| Spaced {
            values: values.iter().map(|&v| Number::Int(v)).collect(),
            note: Some(note),
        },
    )
}

fn decimals(lo: Decimal, hi: Decimal, n: usize, log: bool) -> Spaced {
    let (lo_f, hi_f) = (lo.to_f64().unwrap_or(0.0), hi.to_f64().unwrap_or(0.0));
    let mut values: Vec<Decimal> = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let value = if i == 0 {
            lo
        } else if i == n {
            hi
        } else if log {
            let exact = lo_f * (hi_f / lo_f).powf(i as f64 / n as f64);
            Decimal::from_scientific(&format!("{exact:.5e}")).unwrap_or(lo)
        } else {
            (lo + (hi - lo) * Decimal::from(i) / Decimal::from(n)).round_dp(12)
        };
        values.push(value.normalize());
    }
    let total = values.len();
    values.dedup();
    let removed = total - values.len();
    Spaced {
        values: values.into_iter().map(Number::Decimal).collect(),
        note: note(false, removed, "decimals"),
    }
}

fn note(rounded: bool, removed: usize, what: &str) -> Option<String> {
    match (rounded, removed) {
        (false, 0) => None,
        (true, 0) => Some(format!("the scan values were rounded to {what}")),
        (_, removed) => Some(format!(
            "the scan values were rounded to {what}; {removed} duplicate value{} removed",
            if removed == 1 { "" } else { "s" }
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ints(spaced: &Spaced) -> Vec<i32> {
        spaced
            .values
            .iter()
            .map(|v| match v {
                Number::Int(i) => *i,
                Number::Decimal(_) => panic!("decimal in {spaced:?}"),
            })
            .collect()
    }

    fn texts(spaced: &Spaced) -> Vec<String> {
        spaced.values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn logarithmic_integers_hit_every_power() {
        let s = spaced("10", "1000000", 5, true).unwrap();
        assert_eq!(ints(&s), [10, 100, 1000, 10000, 100000, 1000000]);
        assert_eq!(s.note, None);
    }

    #[test]
    fn logarithmic_integers_are_rounded_and_deduplicated() {
        // 1, 1.78, 3.16, 5.62, 10 -> 1, 2, 3, 6, 10
        let s = spaced("1", "10", 4, true).unwrap();
        assert_eq!(ints(&s), [1, 2, 3, 6, 10]);
        assert!(s.note.unwrap().contains("rounded to integers"));
        // more points than integers: 1, 1.19, 1.41, 1.68, 2 -> 1, 1, 1, 2, 2
        let s = spaced("1", "2", 4, true).unwrap();
        assert_eq!(ints(&s), [1, 2]);
        assert!(s.note.unwrap().contains("duplicate"));
    }

    #[test]
    fn linear_steps() {
        assert_eq!(
            ints(&spaced("0", "100", 4, false).unwrap()),
            [0, 25, 50, 75, 100]
        );
        assert_eq!(
            texts(&spaced("0.5", "1.5", 4, false).unwrap()),
            ["0.5", "0.75", "1", "1.25", "1.5"]
        );
        // 0, 3.33, 6.67, 10 -> integers with a note
        let s = spaced("0", "10", 3, false).unwrap();
        assert_eq!(ints(&s), [0, 3, 7, 10]);
        assert!(s.note.is_some());
        assert_eq!(
            texts(&spaced("0.0", "1", 3, false).unwrap()),
            ["0", "0.333333333333", "0.666666666667", "1"]
        );
    }

    #[test]
    fn decimal_logarithmic_values() {
        let s = spaced("0.001", "1", 3, true).unwrap();
        assert_eq!(texts(&s), ["0.001", "0.01", "0.1", "1"]);
        let s = spaced("1", "2.5", 2, true).unwrap();
        assert_eq!(texts(&s), ["1", "1.58114", "2.5"]);
    }

    #[test]
    fn single_value_and_single_step() {
        assert_eq!(ints(&spaced("7", "7", 3, true).unwrap()), [7]);
        assert_eq!(ints(&spaced("1", "1000", 1, true).unwrap()), [1, 1000]);
    }

    #[test]
    fn errors() {
        assert!(
            matches!(spaced("0", "10", 3, true), Err(ParameterScanError::InvalidSpacing(m)) if m.contains("MIN > 0"))
        );
        assert!(matches!(
            spaced("-1", "10", 3, true),
            Err(ParameterScanError::InvalidSpacing(_))
        ));
        assert!(matches!(
            spaced("0.0", "10", 3, true),
            Err(ParameterScanError::InvalidSpacing(_))
        ));
        assert!(matches!(
            spaced("10", "1", 3, false),
            Err(ParameterScanError::EmptyRange)
        ));
        assert!(matches!(
            spaced("1", "10", 0, false),
            Err(ParameterScanError::InvalidSpacing(_))
        ));
        assert!(matches!(
            spaced("1", "10", 100_001, false),
            Err(ParameterScanError::InvalidSpacing(_))
        ));
        assert!(spaced("a", "10", 3, false).is_err());
    }
}
