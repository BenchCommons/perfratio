use anyhow::{bail, ensure, Result};
use regex::bytes::Regex;
use std::str::FromStr;

/// A user-specified metric to extract from a command's standard output.
#[derive(Debug, Clone)]
pub struct OutputMetric {
    pub name: String,
    pub re: Regex,
    /// Unit label declared with `NAME[unit,...]`
    pub unit: Option<String>,
    /// `true` if declared with `NAME[...,higher]`: a larger value is better
    pub higher_is_better: bool,
}

/// Parses the `[unit,higher]` part of `NAME[unit,higher]`: at most two comma-separated items;
/// `higher` and `lower` set the direction, any other item is the unit.
fn parse_declaration(name: &str, decl: &str) -> Result<(Option<String>, bool)> {
    let (mut unit, mut direction): (Option<String>, Option<bool>) = (None, None);
    let items: Vec<&str> = decl.split(',').map(str::trim).collect();
    ensure!(
        items.len() <= 2,
        "metric '{name}': expected '[unit]', '[higher]' or '[unit,higher]' between the brackets"
    );
    for item in items {
        match item {
            "" => bail!("metric '{name}': empty item between the brackets"),
            "higher" | "lower" => {
                ensure!(
                    direction.is_none(),
                    "metric '{name}': direction declared twice"
                );
                direction = Some(item == "higher");
            }
            unit_label => {
                ensure!(unit.is_none(), "metric '{name}': unit declared twice");
                ensure!(
                    unit_label.chars().count() <= 16
                        && unit_label
                            .chars()
                            .all(|c| !c.is_control() && !c.is_whitespace() && !"[]=,".contains(c)),
                    "metric '{name}': invalid unit '{unit_label}' (at most 16 characters, no spaces, brackets, ',' or '=')"
                );
                unit = Some(unit_label.to_string());
            }
        }
    }
    Ok((unit, direction.unwrap_or(false)))
}

impl PartialEq for OutputMetric {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.re.as_str() == other.re.as_str()
            && self.unit == other.unit
            && self.higher_is_better == other.higher_is_better
    }
}

impl FromStr for OutputMetric {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (name, pattern) = s
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected 'NAME[unit,higher]=REGEX', got '{s}'"))?;
        let (name, unit, higher_is_better) = match name.split_once('[') {
            Some((base, rest)) => {
                let decl = rest.strip_suffix(']').ok_or_else(|| {
                    anyhow::anyhow!("metric '{base}': missing ']' after the unit or direction")
                })?;
                let (unit, higher) = parse_declaration(base, decl)?;
                (base, unit, higher)
            }
            None => (name, None, false),
        };

        ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "invalid metric name '{name}': names must contain only ASCII alphanumeric characters and underscores"
        );

        let re =
            Regex::new(pattern).map_err(|e| anyhow::anyhow!("invalid regex '{pattern}': {e}"))?;

        ensure!(
            re.captures_len() >= 2,
            "regex for metric '{name}' must contain at least one capture group '(...)'"
        );

        Ok(Self {
            name: name.to_string(),
            re,
            unit,
            higher_is_better,
        })
    }
}

impl OutputMetric {
    /// Registers unit and direction, so that ranking and the export see them.
    pub fn declare(&self) {
        crate::metric::set_custom_meta(
            &self.name,
            crate::metric::CustomMeta {
                unit: self.unit.clone(),
                higher_is_better: self.higher_is_better,
            },
        );
    }

    /// Extracts the numeric metric value from the given output bytes.
    /// Returns the parsed f64 from the first capture group of the LAST match.
    pub fn extract(&self, output: &[u8]) -> Option<f64> {
        let caps = self.re.captures_iter(output).last()?;
        let matched_bytes = caps.get(1)?.as_bytes();
        let s = std::str::from_utf8(matched_bytes).ok()?;
        s.trim().parse::<f64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_output_metric_from_str_valid() {
        let m = OutputMetric::from_str("latency=([0-9.]+)ms").unwrap();
        assert_eq!(m.name, "latency");

        let m2 = OutputMetric::from_str("qps_total=Total QPS: ([0-9]+)").unwrap();
        assert_eq!(m2.name, "qps_total");
    }

    #[test]
    fn test_output_metric_from_str_invalid() {
        // No '='
        assert!(OutputMetric::from_str("latency([0-9]+)").is_err());

        // Empty name
        assert!(OutputMetric::from_str("=([0-9]+)").is_err());

        // Invalid name with hyphen
        assert!(OutputMetric::from_str("my-metric=([0-9]+)").is_err());

        // Invalid name with space
        assert!(OutputMetric::from_str("my metric=([0-9]+)").is_err());

        // No capture group
        assert!(OutputMetric::from_str("latency=[0-9]+").is_err());

        // Invalid regex syntax
        assert!(OutputMetric::from_str("latency=([0-9+").is_err());
    }

    #[test]
    fn test_output_metric_unit_and_direction() {
        let m = OutputMetric::from_str("qps[ops/s,higher]=qps: ([0-9.]+)").unwrap();
        assert_eq!(m.name, "qps");
        assert_eq!(m.unit.as_deref(), Some("ops/s"));
        assert!(m.higher_is_better);

        let m = OutputMetric::from_str("lat[ms]=([0-9.]+)").unwrap();
        assert_eq!((m.unit.as_deref(), m.higher_is_better), (Some("ms"), false));

        let m = OutputMetric::from_str("score[higher]=([0-9.]+)").unwrap();
        assert_eq!((m.unit, m.higher_is_better), (None, true));

        let m = OutputMetric::from_str("x[ms,lower]=([0-9]+)").unwrap();
        assert!(!m.higher_is_better);

        // brackets inside the regex are untouched
        let m = OutputMetric::from_str("x[s]=([0-9]+)[a-z]").unwrap();
        assert_eq!(m.re.as_str(), "([0-9]+)[a-z]");
        assert_eq!(OutputMetric::from_str("x=([0-9]+)").unwrap().unit, None);

        for bad in [
            "x[ms=([0-9]+)",
            "x[]=([0-9]+)",
            "x[ms,]=([0-9]+)",
            "x[ms,s]=([0-9]+)",
            "x[higher,lower]=([0-9]+)",
            "x[a,b,c]=([0-9]+)",
            "x[a b]=([0-9]+)",
            "x[averyveryverylongunitlabel]=([0-9]+)",
            "[ms]=([0-9]+)",
        ] {
            assert!(OutputMetric::from_str(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn test_output_metric_extract() {
        let m = OutputMetric::from_str("latency=time=([0-9.]+)s").unwrap();

        // Single match
        let out = b"setup... time=2.5s done";
        assert_eq!(m.extract(out), Some(2.5));

        // Multiple matches -> takes last match
        let out_multi = b"pass 1: time=1.0s\npass 2: time=4.75s\nall done";
        assert_eq!(m.extract(out_multi), Some(4.75));

        // Scientific notation
        let m_sci = OutputMetric::from_str("err=error: ([0-9.eE+-]+)").unwrap();
        assert_eq!(m_sci.extract(b"error: 1.5e-3"), Some(0.0015));

        // Non-numeric capture
        assert_eq!(m.extract(b"time=unknowns"), None);

        // No match
        assert_eq!(m.extract(b"no timings here"), None);
    }
}
