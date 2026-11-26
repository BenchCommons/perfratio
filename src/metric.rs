use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Prefix of the selector for a metric extracted with `--output-metric`
/// (`--metric custom:<name>`).
pub const CUSTOM_PREFIX: &str = "custom:";

/// Interns a custom metric name so that [`Metric`] stays `Copy`. The set of
/// names is bounded by the `--output-metric` names and imported files, so the
/// leaked memory is a few bytes per distinct name.
fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = names.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// Unit and direction declared for a custom metric, with `--output-metric NAME[unit,higher]=REGEX`
/// or by the descriptors of an imported file (#317).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomMeta {
    /// Unit label (`ms`, `ops/s`, ...), `None` for a unitless metric
    pub unit: Option<String>,
    /// `true` if a larger value is better
    pub higher_is_better: bool,
}

fn custom_meta_table(
) -> std::sync::MutexGuard<'static, std::collections::HashMap<String, CustomMeta>> {
    static META: OnceLock<Mutex<std::collections::HashMap<String, CustomMeta>>> = OnceLock::new();
    META.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Declares unit and direction of the custom metric `name`, replacing an earlier declaration.
pub fn set_custom_meta(name: &str, meta: CustomMeta) {
    custom_meta_table().insert(name.to_string(), meta);
}

/// Declares unit and direction of `name` unless it has been declared already (the command
/// line wins over an imported file).
pub fn set_custom_meta_if_absent(name: &str, meta: CustomMeta) {
    custom_meta_table().entry(name.to_string()).or_insert(meta);
}

/// Declared unit and direction of the custom metric `name`.
pub fn custom_meta(name: &str) -> Option<CustomMeta> {
    custom_meta_table().get(name).cloned()
}

/// Target metric used as the primary objective for benchmarking, comparison, and sorting.
///
/// Serialized as its name (`wall`, `cpu`, ..., or `custom:<name>`), see
/// `docs/specs/metric-target.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    #[default]
    Wall,
    Cpu,
    User,
    System,
    Energy,
    Memory,
    /// Retired instructions (`--counters`, Linux); needs a reading for every run.
    Instructions,
    /// CPU cycles (`--counters`, Linux); needs a reading for every run.
    Cycles,
    /// A metric extracted with `--output-metric <name>=<regex>`; lower is better.
    Custom(&'static str),
}

impl Metric {
    /// Selector for the custom metric `name`.
    pub fn custom(name: &str) -> Self {
        Metric::Custom(intern(name))
    }

    /// Name of the custom metric, if this is one.
    pub fn custom_name(&self) -> Option<&'static str> {
        match self {
            Metric::Custom(name) if !name.starts_with(crate::region::METRIC_PREFIX) => Some(name),
            _ => None,
        }
    }

    /// Region selected with `--metric region:<name>`, if this is one. Region
    /// metrics reuse the `Custom` variant under the key `region:<name>`.
    pub fn region_name(&self) -> Option<&'static str> {
        match self {
            Metric::Custom(name) => crate::region::metric_region(name),
            _ => None,
        }
    }

    /// Selector for the region `name` (`--metric region:<name>`).
    pub fn region(name: &str) -> Self {
        Metric::custom(&crate::region::time_key(name))
    }

    /// Whether this metric is the time of an instrumented region.
    pub fn is_region(&self) -> bool {
        self.region_name().is_some()
    }

    /// Canonical lowercase name of the metric (`custom:<name>` is rendered by `Display`)
    pub fn name(&self) -> &'static str {
        match self {
            Metric::Wall => "wall",
            Metric::Cpu => "cpu",
            Metric::User => "user",
            Metric::System => "system",
            Metric::Energy => "energy",
            Metric::Memory => "memory",
            Metric::Instructions => "instructions",
            Metric::Cycles => "cycles",
            Metric::Custom(name) => name,
        }
    }

    /// Header name displayed in the terminal output
    pub fn display_header(&self) -> &'static str {
        match self {
            Metric::Wall => "Time",
            Metric::Cpu => "CPU Time",
            Metric::User => "User Time",
            Metric::System => "System Time",
            Metric::Energy => "Energy",
            Metric::Memory => "Memory",
            Metric::Instructions => "Instructions",
            Metric::Cycles => "Cycles",
            Metric::Custom(name) => name,
        }
    }

    /// Verb used in summary sentences, e.g. "command ran" vs "command used"
    pub fn verb(&self) -> &'static str {
        match self {
            Metric::Wall | Metric::Cpu | Metric::User | Metric::System => "ran",
            Metric::Energy | Metric::Memory | Metric::Instructions | Metric::Cycles => "used",
            Metric::Custom(_) if self.is_region() => "ran",
            Metric::Custom(_) => "had",
        }
    }

    /// Whether a larger value of the metric is better. Only a custom metric declared with
    /// `higher` (or imported with `higher_is_better`) is; every other metric improves downwards.
    pub fn higher_is_better(&self) -> bool {
        self.custom_name()
            .and_then(custom_meta)
            .is_some_and(|m| m.higher_is_better)
    }

    /// Comparison words: (better, worse), e.g. ("faster", "slower") or ("less energy", "more energy")
    pub fn comparison_words(&self) -> (&'static str, &'static str) {
        match self {
            Metric::Wall | Metric::Cpu | Metric::User | Metric::System => ("faster", "slower"),
            Metric::Energy => ("less energy", "more energy"),
            Metric::Memory => ("less memory", "more memory"),
            Metric::Instructions => ("fewer instructions", "more instructions"),
            Metric::Cycles => ("fewer cycles", "more cycles"),
            Metric::Custom(_) if self.is_region() => ("faster", "slower"),
            Metric::Custom(_) if self.higher_is_better() => ("higher value", "lower value"),
            Metric::Custom(_) => ("lower value", "higher value"),
        }
    }

    /// Phrase used when two commands have equal measurements
    pub fn equal_word(&self) -> &'static str {
        match self {
            Metric::Wall | Metric::Cpu | Metric::User | Metric::System => "as fast as",
            Metric::Energy => "same energy as",
            Metric::Memory => "same memory as",
            Metric::Instructions => "the same instructions as",
            Metric::Cycles => "the same cycles as",
            Metric::Custom(_) if self.is_region() => "as fast as",
            Metric::Custom(_) => "the same value as",
        }
    }

    /// The hardware counter behind `instructions` / `cycles`.
    pub fn counter_kind(&self) -> Option<crate::perf_counters::CounterKind> {
        match self {
            Metric::Instructions => Some(crate::perf_counters::CounterKind::Instructions),
            Metric::Cycles => Some(crate::perf_counters::CounterKind::Cycles),
            _ => None,
        }
    }

    /// Whether this metric represents a duration in time
    pub fn is_time(&self) -> bool {
        matches!(
            self,
            Metric::Wall | Metric::Cpu | Metric::User | Metric::System
        ) || self.is_region()
    }
}

impl FromStr for Metric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "wall" | "wall-clock" | "time" => Ok(Metric::Wall),
            "cpu" | "total-cpu" => Ok(Metric::Cpu),
            "user" => Ok(Metric::User),
            "system" => Ok(Metric::System),
            "energy" => Ok(Metric::Energy),
            "memory" | "rss" | "peak-memory" => Ok(Metric::Memory),
            "instructions" => Ok(Metric::Instructions),
            "cycles" => Ok(Metric::Cycles),
            other => {
                // names are case-sensitive, so use the original spelling
                if let Some(name) = s
                    .get(..crate::region::METRIC_PREFIX.len())
                    .filter(|p| p.eq_ignore_ascii_case(crate::region::METRIC_PREFIX))
                    .map(|_| &s[crate::region::METRIC_PREFIX.len()..])
                {
                    if crate::region::is_valid_name(name) {
                        return Ok(Metric::region(name));
                    }
                    return Err(format!(
                        "invalid metric '{s}': expected 'region:<name>' with a name of letters, digits and '_'"
                    ));
                }
                if let Some(name) = s
                    .get(..CUSTOM_PREFIX.len())
                    .filter(|p| p.eq_ignore_ascii_case(CUSTOM_PREFIX))
                    .map(|_| &s[CUSTOM_PREFIX.len()..])
                {
                    if !name.is_empty()
                        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return Ok(Metric::custom(name));
                    }
                    return Err(format!(
                        "invalid metric '{s}': expected 'custom:<name>' with a name of letters, digits and '_'"
                    ));
                }
                Err(format!(
                    "invalid metric '{other}': possible values are wall, cpu, user, system, energy, memory, instructions, cycles, custom:<name>, region:<name>"
                ))
            }
        }
    }
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Metric::Custom(name) if name.starts_with(crate::region::METRIC_PREFIX) => {
                f.write_str(name)
            }
            Metric::Custom(name) => write!(f, "{CUSTOM_PREFIX}{name}"),
            other => f.write_str(other.name()),
        }
    }
}

impl Serialize for Metric {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Metric {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metric_defaults_and_names() {
        assert_eq!(Metric::default(), Metric::Wall);
        assert_eq!(Metric::Wall.name(), "wall");
        assert_eq!(Metric::Cpu.name(), "cpu");
        assert_eq!(Metric::User.name(), "user");
        assert_eq!(Metric::System.name(), "system");
        assert_eq!(Metric::Energy.name(), "energy");
        assert_eq!(Metric::Memory.name(), "memory");
    }

    #[test]
    fn test_metric_verbs_and_comparisons() {
        assert_eq!(Metric::Wall.verb(), "ran");
        assert_eq!(Metric::Energy.verb(), "used");
        assert_eq!(Metric::Memory.verb(), "used");

        assert_eq!(Metric::Wall.comparison_words(), ("faster", "slower"));
        assert_eq!(
            Metric::Energy.comparison_words(),
            ("less energy", "more energy")
        );
        assert_eq!(
            Metric::Memory.comparison_words(),
            ("less memory", "more memory")
        );
    }

    #[test]
    fn test_metric_is_time() {
        assert!(Metric::Wall.is_time());
        assert!(Metric::Cpu.is_time());
        assert!(Metric::User.is_time());
        assert!(Metric::System.is_time());
        assert!(!Metric::Energy.is_time());
        assert!(!Metric::Memory.is_time());
    }

    #[test]
    fn test_metric_from_str() {
        assert_eq!("wall".parse::<Metric>().unwrap(), Metric::Wall);
        assert_eq!("wall-clock".parse::<Metric>().unwrap(), Metric::Wall);
        assert_eq!("time".parse::<Metric>().unwrap(), Metric::Wall);
        assert_eq!("cpu".parse::<Metric>().unwrap(), Metric::Cpu);
        assert_eq!("total-cpu".parse::<Metric>().unwrap(), Metric::Cpu);
        assert_eq!("user".parse::<Metric>().unwrap(), Metric::User);
        assert_eq!("system".parse::<Metric>().unwrap(), Metric::System);
        assert_eq!("energy".parse::<Metric>().unwrap(), Metric::Energy);
        assert_eq!("memory".parse::<Metric>().unwrap(), Metric::Memory);
        assert_eq!("rss".parse::<Metric>().unwrap(), Metric::Memory);
        assert_eq!("peak-memory".parse::<Metric>().unwrap(), Metric::Memory);
        assert_eq!(
            "Instructions".parse::<Metric>().unwrap(),
            Metric::Instructions
        );
        assert_eq!("cycles".parse::<Metric>().unwrap(), Metric::Cycles);
        assert_eq!(Metric::Cycles.to_string(), "cycles");
        assert_eq!(
            Metric::Instructions.counter_kind(),
            Some(crate::perf_counters::CounterKind::Instructions)
        );
        assert_eq!(Metric::Wall.counter_kind(), None);
        assert!("invalid".parse::<Metric>().is_err());
    }

    #[test]
    fn test_region_metric_parse_display_and_serde() {
        let m: Metric = "region:sort".parse().unwrap();
        assert_eq!(m, Metric::region("sort"));
        assert_eq!(m.region_name(), Some("sort"));
        assert_eq!(m.custom_name(), None);
        assert!(m.is_region() && m.is_time());
        assert_eq!(m.to_string(), "region:sort");
        assert_eq!(m.comparison_words(), ("faster", "slower"));
        assert_eq!(serde_json::to_string(&m).unwrap(), "\"region:sort\"");
        assert_eq!(
            serde_json::from_str::<Metric>("\"region:sort\"").unwrap(),
            m
        );
        assert!("region:".parse::<Metric>().is_err());
        assert!("region:a-b".parse::<Metric>().is_err());
        assert_eq!(Metric::custom("x").region_name(), None);
        assert_eq!(Metric::Wall.region_name(), None);
    }

    #[test]
    fn test_custom_metric_parse_display_and_serde() {
        let m: Metric = "custom:latency_ms".parse().unwrap();
        assert_eq!(m, Metric::custom("latency_ms"));
        assert_eq!(m.custom_name(), Some("latency_ms"));
        assert_eq!(m.to_string(), "custom:latency_ms");
        assert_eq!(m.display_header(), "latency_ms");
        assert_eq!(m.verb(), "had");
        assert!(!m.is_time());
        // the name keeps its case
        assert_eq!(
            "custom:Lat".parse::<Metric>().unwrap().to_string(),
            "custom:Lat"
        );
        assert!("custom:".parse::<Metric>().is_err());
        assert!("custom:a-b".parse::<Metric>().is_err());

        assert_eq!(serde_json::to_string(&m).unwrap(), "\"custom:latency_ms\"");
        assert_eq!(
            serde_json::from_str::<Metric>("\"custom:latency_ms\"").unwrap(),
            m
        );
        assert_eq!(serde_json::to_string(&Metric::Cpu).unwrap(), "\"cpu\"");
        assert_eq!(
            serde_json::from_str::<Metric>("\"memory\"").unwrap(),
            Metric::Memory
        );
    }
}
