//! CPU identification from `/proc/cpuinfo` (vendor, family, model).
//!
//! Used to pick raw PMU events from an allow-list (never by probing, see
//! `docs/specs/pmu-extensions-common.md`). The parser is pure and takes the
//! file's text, so it is tested on every platform with fixtures.

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Amd,
    /// Arm, anything not x86, or an unreadable file
    #[default]
    Other,
}

impl fmt::Display for Vendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Vendor::Intel => "Intel",
            Vendor::Amd => "AMD",
            Vendor::Other => "unknown vendor",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuId {
    pub vendor: Vendor,
    pub family: u32,
    pub model: u32,
    /// `/sys/devices/cpu_core` exists: Intel hybrid part with a P-core PMU
    pub hybrid_pmu: bool,
}

impl fmt::Display for CpuId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} family {:#x} model {:#x}",
            self.vendor, self.family, self.model
        )
    }
}

/// Parses the first processor block of `/proc/cpuinfo` (x86 layout:
/// `vendor_id`, `cpu family`, `model`, all but the vendor in decimal).
/// Missing or malformed fields give [`Vendor::Other`] with family and model 0.
pub fn parse_cpuinfo(text: &str) -> CpuId {
    let mut vendor = None;
    let mut family = None;
    let mut model = None;
    for line in text.lines() {
        if line.trim().is_empty() && vendor.is_some() {
            break; // end of the first processor block
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "vendor_id" if vendor.is_none() => {
                vendor = Some(match value {
                    "GenuineIntel" => Vendor::Intel,
                    "AuthenticAMD" => Vendor::Amd,
                    _ => Vendor::Other,
                })
            }
            "cpu family" if family.is_none() => family = value.parse().ok(),
            "model" if model.is_none() => model = value.parse().ok(),
            _ => {}
        }
    }
    match (vendor, family, model) {
        (Some(vendor), Some(family), Some(model)) => CpuId {
            vendor,
            family,
            model,
            hybrid_pmu: false,
        },
        _ => CpuId::default(),
    }
}

/// Reads the running machine. Linux only; elsewhere an unknown CPU.
pub fn detect() -> CpuId {
    #[cfg(target_os = "linux")]
    {
        let mut id = std::fs::read_to_string("/proc/cpuinfo")
            .map(|t| parse_cpuinfo(&t))
            .unwrap_or_default();
        id.hybrid_pmu = std::path::Path::new("/sys/devices/cpu_core").exists();
        id
    }
    #[cfg(not(target_os = "linux"))]
    {
        CpuId::default()
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    /// A `/proc/cpuinfo` excerpt (two processors, to check that only the first counts).
    pub fn cpuinfo(vendor: &str, family: u32, model: u32) -> String {
        let block = |n: u32| {
            format!(
                "processor\t: {n}\nvendor_id\t: {vendor}\ncpu family\t: {family}\nmodel\t\t: {model}\n\
                 model name\t: Some CPU\nstepping\t: 7\n\n"
            )
        };
        format!("{}{}", block(0), block(1))
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::cpuinfo;
    use super::*;

    #[test]
    fn parses_intel_and_amd() {
        let id = parse_cpuinfo(&cpuinfo("GenuineIntel", 6, 85));
        assert_eq!(
            (id.vendor, id.family, id.model),
            (Vendor::Intel, 6, 0x55),
            "{id}"
        );
        let id = parse_cpuinfo(&cpuinfo("AuthenticAMD", 25, 17));
        assert_eq!((id.vendor, id.family, id.model), (Vendor::Amd, 25, 0x11));
        assert_eq!(id.to_string(), "AMD family 0x19 model 0x11");
    }

    #[test]
    fn model_name_is_not_the_model() {
        let text = "vendor_id\t: GenuineIntel\nmodel name\t: Intel(R) Xeon(R)\ncpu family\t: 6\nmodel\t\t: 143\n";
        assert_eq!(parse_cpuinfo(text).model, 143);
    }

    #[test]
    fn unknown_or_arm_text_is_other() {
        assert_eq!(parse_cpuinfo(""), CpuId::default());
        let arm = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x41\nCPU part\t: 0xd0c\n";
        assert_eq!(parse_cpuinfo(arm).vendor, Vendor::Other);
        let odd = cpuinfo("HygonGenuine", 24, 0);
        assert_eq!(parse_cpuinfo(&odd).vendor, Vendor::Other);
        assert_eq!(
            parse_cpuinfo("vendor_id : GenuineIntel\ncpu family : x\nmodel : 1\n"),
            CpuId::default()
        );
    }
}
