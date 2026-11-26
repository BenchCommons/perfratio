//! `--simd`: floating-point scalar/vector breakdown from raw PMU events (#211).
//!
//! The event tables, the CPU allow-list, the vectorization ratio and the
//! export are pure functions, tested on every platform. Only the final
//! `perf_event_open` call (in [`crate::perf_counters`]) is Linux specific.
//! See `docs/specs/simd-breakdown.md`.
//!
//! Events (checked against perf's `tools/perf/pmu-events/arch/x86`):
//! - Intel `FP_ARITH_INST_RETIRED` (event `0xC7`). The umask is a bit mask the
//!   kernel ORs, so four events cover scalar and the three packed widths,
//!   single and double precision together. Each count is one instruction,
//!   FMA counts twice.
//! - AMD Zen 4 and 5 `fp_ops_retired_by_width` (event `0x08`), counted in uops.
//! - AArch64 is **not supported**: the per-width events do not exist, and the
//!   spec's `ASE_SPEC` (`0x74`) and `VFP_SPEC` (`0x75`) are implementation
//!   defined in the kernel (`arm_pmuv3.h`, `ARMV8_IMPDEF_*`), not in perf's
//!   common arm64 JSON. Only `SVE_INST_SPEC` (`0x8006`) is verified. Without a
//!   verified event set the result is `unavailable (unsupported_cpu)`.

use crate::benchmark::benchmark_result::MetricSummary;
use crate::cpu_id::{CpuId, Vendor};
use crate::perf_counters::{Scope, UnavailableReason};
use crate::pmu::{self, PmuStatus};
use crate::pmu_scaling::{multiplex_warning, ScaledCounter};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Bucket {
    ScalarFp,
    Simd128,
    Simd256,
    Simd512,
}

impl Bucket {
    /// JSON key
    pub fn name(self) -> &'static str {
        match self {
            Bucket::ScalarFp => "scalar_fp",
            Bucket::Simd128 => "simd_128",
            Bucket::Simd256 => "simd_256",
            Bucket::Simd512 => "simd_512",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Bucket::ScalarFp => "Scalar FP:",
            Bucket::Simd128 => "128-bit SIMD:",
            Bucket::Simd256 => "256-bit SIMD:",
            Bucket::Simd512 => "512-bit SIMD:",
        }
    }

    fn is_vector(self) -> bool {
        self != Bucket::ScalarFp
    }
}

/// What one count means. Intel and AMD counts are not comparable in absolute terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Instructions,
    Uops,
}

impl Unit {
    pub fn as_str(self) -> &'static str {
        match self {
            Unit::Instructions => "instructions",
            Unit::Uops => "uops",
        }
    }
}

/// `PERF_TYPE_RAW`
pub const PERF_TYPE_RAW: u32 = 4;

/// `perf_event_attr.config` of a raw x86 event: event select bits 0-7 and
/// 32-35 (AMD's 12-bit event number), unit mask bits 8-15.
pub const fn raw_config(event: u16, umask: u8) -> u64 {
    (event as u64 & 0xff) | ((umask as u64) << 8) | (((event as u64 >> 8) & 0xf) << 32)
}

/// The events chosen for a CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub arch: &'static str,
    pub unit: Unit,
    /// Fixed per model; a bucket the CPU lacks is absent, not zero.
    pub events: Vec<(Bucket, u64)>,
}

// Intel family 6 models whose perf JSON has `FP_ARITH_INST_RETIRED.*`
// (mapfile.csv: broadwell, skylake, skylakex/cascadelakex, icelake(x),
// tigerlake, rocketlake, sapphirerapids, emeraldrapids, graniterapids).
// Haswell has no such event.
const INTEL_NO_AVX512: [u32; 10] = [
    0x3D, 0x47, 0x4F, 0x56, // Broadwell
    0x4E, 0x5E, 0x8E, 0x9E, 0xA5, 0xA6, // Skylake client
];
const INTEL_AVX512: [u32; 12] = [
    0x55, // Skylake-X, Cascade Lake, Cooper Lake
    0x7D, 0x7E, 0x6A, 0x6C, // Ice Lake
    0x8C, 0x8D, // Tiger Lake
    0xA7, // Rocket Lake
    0x8F, 0xCF, // Sapphire Rapids, Emerald Rapids
    0xAD, 0xAE, // Granite Rapids
];
// Hybrid parts: the event lives on the `cpu_core` PMU and E-cores lack the
// 256/512-bit buckets (alderlake, meteorlake, arrowlake, lunarlake).
const INTEL_HYBRID: [u32; 11] = [
    0x97, 0x9A, 0xB7, 0xBA, 0xBF, 0xAA, 0xAC, 0xB5, 0xC5, 0xC6, 0xBD,
];

/// AMD `fp_ops_retired_by_width` is in perf's amdzen4 and amdzen5 JSON only.
/// perf's `mapfile.csv` regexes (model in hex): Zen 3 is family 0x19 models
/// 0x00-0x0F, 0x20-0x2F, 0x40-0x5F, every other 0x19 model is Zen 4; Zen 5 is
/// family 0x1a models 0x00-0x2F, 0x40-0x4F, 0x60-0x7F. Later models are not
/// in the allow-list until their JSON is checked.
fn amd_has_width_event(family: u32, model: u32) -> bool {
    match family {
        0x19 => {
            !(model <= 0x0F || (0x20..=0x2F).contains(&model) || (0x40..=0x5F).contains(&model))
        }
        0x1a => model <= 0x2F || (0x40..=0x4F).contains(&model) || (0x60..=0x7F).contains(&model),
        _ => false,
    }
}

/// Picks the event set for a CPU, or says why there is none.
pub fn select(cpu: &CpuId) -> Result<Selection, UnavailableReason> {
    match cpu.vendor {
        Vendor::Intel if cpu.family == 6 => {
            if cpu.hybrid_pmu || INTEL_HYBRID.contains(&cpu.model) {
                return Err(UnavailableReason::HybridCpuUnpinned);
            }
            let has_512 = INTEL_AVX512.contains(&cpu.model);
            if !has_512 && !INTEL_NO_AVX512.contains(&cpu.model) {
                return Err(UnavailableReason::UnsupportedCpu);
            }
            let mut events = vec![
                (Bucket::ScalarFp, raw_config(0xC7, 0x03)),
                (Bucket::Simd128, raw_config(0xC7, 0x0c)),
                (Bucket::Simd256, raw_config(0xC7, 0x30)),
            ];
            if has_512 {
                events.push((Bucket::Simd512, raw_config(0xC7, 0xc0)));
            }
            Ok(Selection {
                arch: "x86_64",
                unit: Unit::Instructions,
                events,
            })
        }
        Vendor::Amd if amd_has_width_event(cpu.family, cpu.model) => Ok(Selection {
            arch: "x86_64",
            unit: Unit::Uops,
            events: vec![
                (Bucket::ScalarFp, raw_config(0x08, 0x04)),
                (Bucket::Simd128, raw_config(0x08, 0x08)),
                (Bucket::Simd256, raw_config(0x08, 0x10)),
                (Bucket::Simd512, raw_config(0x08, 0x20)),
            ],
        }),
        _ => Err(UnavailableReason::UnsupportedCpu),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Measured,
    Unavailable(UnavailableReason),
}

/// The resolved `--simd` request. `events` is empty unless `Measured`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimdState {
    pub arch: &'static str,
    pub unit: Unit,
    pub events: Vec<(Bucket, u64)>,
    pub status: Status,
    /// Privilege levels counted (`simd_scope`)
    pub scope: Scope,
}

impl SimdState {
    pub fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            arch: "",
            unit: Unit::Instructions,
            events: vec![],
            status: Status::Unavailable(reason),
            scope: Scope::All,
        }
    }

    /// `--debug-mode`: a fixed Intel AVX-512 event set, whatever the host is.
    pub fn mock(scope: Scope) -> Self {
        let cpu = CpuId {
            vendor: Vendor::Intel,
            family: 6,
            model: 0x55,
            hybrid_pmu: false,
        };
        let s = select(&cpu).expect("Skylake-X is in the allow-list");
        Self {
            arch: s.arch,
            unit: s.unit,
            events: s.events,
            status: Status::Measured,
            scope,
        }
    }
}

const EPERM: i32 = 1;
const EACCES: i32 = 13;

/// Pure planning step. `requested_scope` is the default scope of `--scope`;
/// `settled` is the scope `--counters` already ended up with (so the fallback
/// warning appears once). `probe` tries to open one raw event. Returns the
/// state and the warnings.
pub fn plan(
    cpu: &CpuId,
    paranoid: Option<i32>,
    requested_scope: Scope,
    settled: Option<Scope>,
    mut probe: impl FnMut(u64, Scope) -> Result<(), i32>,
) -> (SimdState, Vec<String>) {
    let mut scope = settled.unwrap_or(requested_scope);
    let selection = match select(cpu) {
        Ok(s) => s,
        Err(reason) => {
            let why = match reason {
                UnavailableReason::HybridCpuUnpinned => format!(
                    "hybrid CPUs ({cpu}, P and E cores) are not supported yet, because the \
                     events exist only on the P-core PMU"
                ),
                _ => format!("no verified floating-point width events for this CPU ({cpu})"),
            };
            return (
                SimdState::unavailable(reason),
                vec![format!(
                    "'--simd' is not available: {why}. Continuing without the SIMD breakdown."
                )],
            );
        }
    };
    let mut warnings = Vec::new();
    if settled.is_none() && scope == Scope::All {
        if let Some(&(_, first)) = selection.events.first() {
            if matches!(probe(first, Scope::All), Err(EPERM | EACCES)) {
                scope = Scope::User;
                warnings.push(format!(
                    "'--simd' counts user-space events only (kernel counting is denied, \
                     perf_event_paranoid={}).",
                    paranoid.map_or("unknown".to_string(), |p| p.to_string())
                ));
            }
        }
    }
    // All or nothing: a missing bucket would skew the vectorization ratio.
    for &(_, config) in &selection.events {
        if let Err(errno) = probe(config, scope) {
            let status = pmu::decide(paranoid, Some(pmu::Probe::Failed(errno)));
            let reason = match status {
                PmuStatus::Denied { .. } => UnavailableReason::PerfParanoid,
                PmuStatus::Unsupported => UnavailableReason::NoPmu,
                _ => UnavailableReason::Other,
            };
            warnings.push(format!(
                "'--simd' is not available, continuing without the SIMD breakdown: {}",
                pmu::diagnostic(&status, "perfratio")
                    .unwrap_or_else(|| pmu::summary(&status))
                    .trim_end()
            ));
            return (SimdState::unavailable(reason), warnings);
        }
    }
    (
        SimdState {
            arch: selection.arch,
            unit: selection.unit,
            events: selection.events,
            status: Status::Measured,
            scope,
        },
        warnings,
    )
}

/// Deterministic per-second rates for `--debug-mode` (a vectorized kernel).
pub fn mock_per_second(bucket: Bucket) -> f64 {
    match bucket {
        Bucket::ScalarFp => 1_200_000.0,
        Bucket::Simd128 => 100_000.0,
        Bucket::Simd256 => 32_400_000.0,
        Bucket::Simd512 => 0.0,
    }
}

/// `vector / (scalar + vector)`; `None` when no floating-point arithmetic retired.
pub fn vectorization_ratio(scalar: u64, vector: u64) -> Option<f64> {
    let total = scalar as f64 + vector as f64;
    (total > 0.0).then(|| vector as f64 / total)
}

/// Per-run series, aligned with `times`; `null` where a run could not be read.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimdSeries {
    /// `instructions` (Intel) or `uops` (AMD)
    pub unit: String,
    pub arch: String,
    pub scalar_fp: Vec<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd_128: Option<Vec<Option<u64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd_256: Option<Vec<Option<u64>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simd_512: Option<Vec<Option<u64>>>,
    /// `null` for a run without floating-point arithmetic or without a reading
    pub vectorization_ratio: Vec<Option<f64>>,
}

impl SimdSeries {
    /// Counts of one bucket, if the CPU has it.
    pub fn bucket(&self, bucket: Bucket) -> Option<&Vec<Option<u64>>> {
        match bucket {
            Bucket::ScalarFp => Some(&self.scalar_fp),
            Bucket::Simd128 => self.simd_128.as_ref(),
            Bucket::Simd256 => self.simd_256.as_ref(),
            Bucket::Simd512 => self.simd_512.as_ref(),
        }
    }
}

/// Multiplexing of one bucket (`simd_scaling`): runs scaled and the lowest coverage.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SimdScaling {
    pub scaled_runs: usize,
    pub coverage: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimdExport {
    pub series: SimdSeries,
    pub summary: BTreeMap<String, MetricSummary>,
    pub scaling: BTreeMap<String, SimdScaling>,
    /// Terminal block, one entry per line
    pub block: Vec<String>,
    pub warning: Option<String>,
}

/// Builds the export. `runs` has one entry per run; each reading holds the
/// basic counters first, then the SIMD events starting at `offset`.
pub fn export(
    state: &SimdState,
    offset: usize,
    runs: &[Option<Vec<Option<ScaledCounter>>>],
) -> Option<SimdExport> {
    if state.status != Status::Measured || state.events.is_empty() || runs.is_empty() {
        return None;
    }
    let cell = |run: &Option<Vec<Option<ScaledCounter>>>, i: usize| {
        run.as_ref()
            .and_then(|r| r.get(offset + i).copied().flatten())
    };
    let counts: Vec<(Bucket, Vec<Option<ScaledCounter>>)> = state
        .events
        .iter()
        .enumerate()
        .map(|(i, &(bucket, _))| (bucket, runs.iter().map(|r| cell(r, i)).collect()))
        .collect();
    let values = |bucket: Bucket| -> Option<Vec<Option<u64>>> {
        counts
            .iter()
            .find(|(b, _)| *b == bucket)
            .map(|(_, cells)| cells.iter().map(|c| c.map(|c| c.value)).collect())
    };
    let scalar_fp = values(Bucket::ScalarFp)?;
    let vector_values: Vec<Vec<Option<u64>>> = counts
        .iter()
        .filter(|(b, _)| b.is_vector())
        .map(|(b, _)| values(*b).unwrap_or_default())
        .collect();
    let vectorization_ratio: Vec<Option<f64>> = (0..runs.len())
        .map(|r| {
            let scalar = scalar_fp[r]?;
            // Every present bucket must be readable, or the ratio would be wrong
            let vector = vector_values
                .iter()
                .map(|v| v[r])
                .try_fold(0u64, |acc, v| v.map(|v| acc.saturating_add(v)))?;
            vectorization_ratio(scalar, vector)
        })
        .collect();
    let series = SimdSeries {
        unit: state.unit.as_str().to_string(),
        arch: state.arch.to_string(),
        scalar_fp,
        simd_128: values(Bucket::Simd128),
        simd_256: values(Bucket::Simd256),
        simd_512: values(Bucket::Simd512),
        vectorization_ratio,
    };

    let mut summary = BTreeMap::new();
    let mut scaling = BTreeMap::new();
    let mut warn_input: Vec<(&str, Vec<ScaledCounter>)> = Vec::new();
    for (bucket, cells) in &counts {
        let floats: Vec<f64> = series
            .bucket(*bucket)
            .into_iter()
            .flatten()
            .flatten()
            .map(|&v| v as f64)
            .collect();
        if let Some(s) = MetricSummary::from_values(&floats) {
            summary.insert(bucket.name().to_string(), s);
        }
        let present: Vec<ScaledCounter> = cells.iter().flatten().copied().collect();
        let scaled: Vec<&ScaledCounter> = present.iter().filter(|c| c.scaled()).collect();
        if !scaled.is_empty() {
            scaling.insert(
                bucket.name().to_string(),
                SimdScaling {
                    scaled_runs: scaled.len(),
                    coverage: scaled.iter().map(|c| c.coverage()).fold(1.0, f64::min),
                },
            );
        }
        warn_input.push((bucket.name(), present));
    }
    let ratios: Vec<f64> = series
        .vectorization_ratio
        .iter()
        .flatten()
        .copied()
        .collect();
    if let Some(s) = MetricSummary::from_values(&ratios) {
        summary.insert("vectorization_ratio".to_string(), s);
    }
    let block = terminal_block(state, &series, &summary);
    Some(SimdExport {
        series,
        summary,
        scaling,
        block,
        warning: multiplex_warning(&warn_input),
    })
}

const LABEL_WIDTH: usize = 23;

fn terminal_block(
    state: &SimdState,
    series: &SimdSeries,
    summary: &BTreeMap<String, MetricSummary>,
) -> Vec<String> {
    let mut lines = vec![format!(
        "  SIMD / FP breakdown ({}, {}):",
        state.arch,
        state.unit.as_str()
    )];
    for &(bucket, _) in &state.events {
        let Some(s) = summary.get(bucket.name()) else {
            continue;
        };
        let value = match s.stddev {
            Some(sd) => format!(
                "{} ± {}",
                crate::benchmark::format_count(s.mean),
                crate::benchmark::format_count(sd)
            ),
            None => crate::benchmark::format_count(s.mean),
        };
        lines.push(format!("    {:<LABEL_WIDTH$}{value}", bucket.label()));
    }
    match summary.get("vectorization_ratio") {
        Some(r) => lines.push(format!(
            "    {:<LABEL_WIDTH$}{:.1}%",
            "Vectorization ratio:",
            r.mean * 100.0
        )),
        None if series.scalar_fp.iter().any(Option::is_some) => lines.push(
            "    no floating-point arithmetic retired (integer SIMD is not counted)".to_string(),
        ),
        None => {}
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu_id::{fixtures::cpuinfo, parse_cpuinfo};
    use crate::pmu_scaling::RawReading;

    fn intel(model: u32) -> CpuId {
        parse_cpuinfo(&cpuinfo("GenuineIntel", 6, model))
    }
    fn amd(family: u32, model: u32) -> CpuId {
        parse_cpuinfo(&cpuinfo("AuthenticAMD", family, model))
    }
    fn buckets(s: &Selection) -> Vec<&'static str> {
        s.events.iter().map(|(b, _)| b.name()).collect()
    }

    #[test]
    fn raw_config_encoding() {
        assert_eq!(raw_config(0xC7, 0x03), 0x03C7);
        assert_eq!(raw_config(0xC7, 0xc0), 0xC0C7);
        assert_eq!(raw_config(0x08, 0x20), 0x2008);
        // AMD event numbers above 0xff put bits 8-11 in config bits 32-35
        assert_eq!(raw_config(0x1AB, 0x01), 0x1_0000_01AB);
    }

    #[test]
    fn intel_event_tables() {
        // Skylake client (0x5E): no AVX-512
        let skl = select(&intel(0x5E)).unwrap();
        assert_eq!((skl.arch, skl.unit), ("x86_64", Unit::Instructions));
        assert_eq!(buckets(&skl), ["scalar_fp", "simd_128", "simd_256"]);
        let configs: Vec<u64> = skl.events.iter().map(|e| e.1).collect();
        assert_eq!(configs, [0x03C7, 0x0CC7, 0x30C7]);
        // Skylake-X, Ice Lake, Sapphire Rapids: with AVX-512
        for model in [0x55, 0x7E, 0x6A, 0x8F] {
            let s = select(&intel(model)).unwrap();
            assert_eq!(buckets(&s)[3], "simd_512", "model {model:#x}");
            assert_eq!(s.events[3].1, 0xC0C7);
        }
        // Broadwell has the event (perf broadwell/floating-point.json)
        assert!(select(&intel(0x3D)).is_ok());
    }

    #[test]
    fn intel_unsupported_models() {
        // Haswell: no FP_ARITH_INST_RETIRED
        for model in [0x3C, 0x45, 0x3F, 0x37, 0x5C] {
            assert_eq!(
                select(&intel(model)),
                Err(UnavailableReason::UnsupportedCpu),
                "{model:#x}"
            );
        }
        // Hybrid: Alder Lake, Raptor Lake, Meteor Lake
        for model in [0x97, 0x9A, 0xB7, 0xAA] {
            assert_eq!(
                select(&intel(model)),
                Err(UnavailableReason::HybridCpuUnpinned)
            );
        }
        // Unknown model on a hybrid PMU
        let mut cpu = intel(0x5E);
        cpu.hybrid_pmu = true;
        assert_eq!(select(&cpu), Err(UnavailableReason::HybridCpuUnpinned));
        // Not family 6
        let mut cpu = intel(0x5E);
        cpu.family = 15;
        assert_eq!(select(&cpu), Err(UnavailableReason::UnsupportedCpu));
    }

    #[test]
    fn amd_event_tables() {
        for (family, model) in [
            (0x19, 0x11),
            (0x19, 0x61),
            (0x19, 0x75),
            (0x1a, 0x02),
            (0x1a, 0x44),
        ] {
            let s = select(&amd(family, model))
                .unwrap_or_else(|e| panic!("{family:#x}/{model:#x}: {e:?}"));
            assert_eq!(s.unit, Unit::Uops);
            let configs: Vec<u64> = s.events.iter().map(|e| e.1).collect();
            assert_eq!(configs, [0x0408, 0x0808, 0x1008, 0x2008]);
        }
    }

    #[test]
    fn amd_zen2_zen3_and_unknown_are_unsupported() {
        for (family, model) in [
            (0x17, 0x31),
            (0x19, 0x01), // Milan, Zen 3
            (0x19, 0x21), // Vermeer, Zen 3
            (0x19, 0x50), // Cezanne, Zen 3
            (0x1a, 0x90), // beyond the verified Zen 5 ranges
            (0x18, 0x00),
        ] {
            assert_eq!(
                select(&amd(family, model)),
                Err(UnavailableReason::UnsupportedCpu),
                "{family:#x}/{model:#x}"
            );
        }
    }

    #[test]
    fn other_vendors_and_arm_are_unsupported() {
        assert_eq!(
            select(&CpuId::default()),
            Err(UnavailableReason::UnsupportedCpu)
        );
        assert_eq!(
            select(&parse_cpuinfo(&cpuinfo("HygonGenuine", 24, 0))),
            Err(UnavailableReason::UnsupportedCpu)
        );
    }

    #[test]
    fn ratio_maths() {
        assert_eq!(vectorization_ratio(0, 0), None);
        assert_eq!(vectorization_ratio(10, 0), Some(0.0));
        assert_eq!(vectorization_ratio(0, 10), Some(1.0));
        // The issue's example: 1.2M scalar, 0.1M + 32.4M vector = 96.4 %
        let r = vectorization_ratio(1_200_000, 100_000 + 32_400_000).unwrap();
        assert!((r - 0.9644).abs() < 1e-3, "{r}");
        assert!(vectorization_ratio(u64::MAX, u64::MAX).unwrap().is_finite());
    }

    #[test]
    fn plan_measures_when_all_events_open() {
        let mut opened = Vec::new();
        let (state, warnings) = plan(&intel(0x55), Some(1), Scope::All, None, |c, s| {
            opened.push((c, s));
            Ok(())
        });
        assert_eq!(state.status, Status::Measured);
        assert_eq!(state.events.len(), 4);
        assert_eq!(state.scope, Scope::All);
        assert!(warnings.is_empty());
        // one fallback probe plus one per event
        assert_eq!(opened.len(), 5);
    }

    #[test]
    fn plan_falls_back_to_user_scope_once() {
        let probe = |_: u64, s: Scope| if s == Scope::All { Err(13) } else { Ok(()) };
        let (state, warnings) = plan(&intel(0x55), Some(2), Scope::All, None, probe);
        assert_eq!((state.status, state.scope), (Status::Measured, Scope::User));
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("perf_event_paranoid=2"),
            "{warnings:?}"
        );
        // `--counters` already fell back: no second warning, no extra probe
        let (state, warnings) = plan(&intel(0x55), Some(2), Scope::All, Some(Scope::User), probe);
        assert_eq!((state.status, state.scope), (Status::Measured, Scope::User));
        assert!(warnings.is_empty());
    }

    #[test]
    fn plan_honors_an_explicit_scope() {
        let mut seen = Vec::new();
        let (state, _) = plan(&intel(0x55), Some(0), Scope::User, None, |_, s| {
            seen.push(s);
            Ok(())
        });
        assert_eq!(state.scope, Scope::User);
        assert!(seen.iter().all(|&s| s == Scope::User));
        // `--scope kernel` denied: unavailable, not a silent downgrade
        let (state, warnings) = plan(&intel(0x55), Some(2), Scope::Kernel, None, |_, s| {
            if s == Scope::Kernel {
                Err(13)
            } else {
                Ok(())
            }
        });
        assert_eq!(
            state.status,
            Status::Unavailable(UnavailableReason::PerfParanoid)
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn plan_reports_unavailable_reasons() {
        for (errno, reason) in [
            (13, UnavailableReason::PerfParanoid),
            (2, UnavailableReason::NoPmu),
            (95, UnavailableReason::NoPmu),
            (22, UnavailableReason::Other),
        ] {
            let (state, warnings) = plan(
                &intel(0x55),
                Some(3),
                Scope::All,
                Some(Scope::User),
                |_, _| Err(errno),
            );
            assert_eq!(state.status, Status::Unavailable(reason), "{errno}");
            assert!(state.events.is_empty());
            assert_eq!(warnings.len(), 1);
        }
    }

    #[test]
    fn plan_partial_failure_is_all_or_nothing() {
        let (state, warnings) = plan(
            &intel(0x55),
            Some(1),
            Scope::All,
            Some(Scope::All),
            |c, _| {
                if c == raw_config(0xC7, 0xc0) {
                    Err(2)
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(state.status, Status::Unavailable(UnavailableReason::NoPmu));
        assert!(state.events.is_empty());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn plan_unsupported_cpu_never_probes() {
        let (state, warnings) = plan(&intel(0x3C), Some(1), Scope::All, None, |_, _| {
            panic!("an unknown model must not be probed")
        });
        assert_eq!(
            state.status,
            Status::Unavailable(UnavailableReason::UnsupportedCpu)
        );
        assert!(
            warnings[0].contains("Intel family 0x6 model 0x3c"),
            "{warnings:?}"
        );
        let (state, warnings) = plan(
            &intel(0x97),
            Some(1),
            Scope::All,
            None,
            |_, _| unreachable!(),
        );
        assert_eq!(
            state.status,
            Status::Unavailable(UnavailableReason::HybridCpuUnpinned)
        );
        assert!(warnings[0].contains("hybrid"));
    }

    fn sc(value: u64, enabled: u64, running: u64) -> Option<ScaledCounter> {
        Some(ScaledCounter::from_raw(RawReading {
            value,
            time_enabled: enabled,
            time_running: running,
        }))
    }

    /// Reading: one basic counter first (offset 1), then scalar, 128, 256, 512.
    fn run(scalar: u64, v128: u64, v256: u64, v512: u64) -> Option<Vec<Option<ScaledCounter>>> {
        Some(vec![
            sc(999, 1, 1),
            sc(scalar, 1, 1),
            sc(v128, 1, 1),
            sc(v256, 1, 1),
            sc(v512, 1, 1),
        ])
    }

    #[test]
    fn export_series_summary_and_block() {
        let state = SimdState::mock(Scope::All);
        let runs = vec![
            run(1_000_000, 100_000, 32_000_000, 0),
            None,
            run(1_400_000, 100_000, 33_000_000, 0),
        ];
        let e = export(&state, 1, &runs).unwrap();
        assert_eq!(e.series.unit, "instructions");
        assert_eq!(
            e.series.scalar_fp,
            vec![Some(1_000_000), None, Some(1_400_000)]
        );
        assert_eq!(e.series.simd_512, Some(vec![Some(0), None, Some(0)]));
        assert_eq!(e.series.vectorization_ratio[1], None);
        let r0 = e.series.vectorization_ratio[0].unwrap();
        assert!((r0 - 32_100_000.0 / 33_100_000.0).abs() < 1e-12);
        assert_eq!(e.summary["scalar_fp"].mean, 1_200_000.0);
        assert!(e.summary["vectorization_ratio"].mean > 0.96);
        assert!(e.scaling.is_empty() && e.warning.is_none());
        let block = e.block.join("\n");
        assert!(
            block.starts_with("  SIMD / FP breakdown (x86_64, instructions):\n"),
            "{block}"
        );
        assert!(
            block.contains("    Scalar FP:             1.2M ± 282.8k"),
            "{block}"
        );
        assert!(block.contains("512-bit SIMD:"), "{block}");
        assert!(block.contains("Vectorization ratio:   96."), "{block}");
    }

    #[test]
    fn export_bucket_absent_when_cpu_lacks_it() {
        let cpu = intel(0x5E);
        let s = select(&cpu).unwrap();
        let state = SimdState {
            arch: s.arch,
            unit: s.unit,
            events: s.events,
            status: Status::Measured,
            scope: Scope::All,
        };
        let runs = vec![Some(vec![sc(10, 1, 1), sc(5, 1, 1), sc(85, 1, 1)])];
        let e = export(&state, 0, &runs).unwrap();
        assert!(e.series.simd_512.is_none());
        let json = serde_json::to_value(&e.series).unwrap();
        assert!(json.get("simd_512").is_none());
        assert_eq!(json["simd_128"], serde_json::json!([5]));
        assert_eq!(json["vectorization_ratio"], serde_json::json!([0.9]));
        assert!(!e.block.join("\n").contains("512"));
    }

    #[test]
    fn export_without_fp_arithmetic_says_so() {
        let state = SimdState::mock(Scope::All);
        let e = export(&state, 0, &[Some(vec![sc(0, 1, 1); 4])]).unwrap();
        assert_eq!(e.series.vectorization_ratio, vec![None]);
        assert!(!e.summary.contains_key("vectorization_ratio"));
        let block = e.block.join("\n");
        assert!(
            block.contains("no floating-point arithmetic retired"),
            "{block}"
        );
        assert!(!block.contains("Vectorization ratio"));
    }

    #[test]
    fn export_ratio_needs_every_bucket() {
        let state = SimdState::mock(Scope::All);
        let mut broken = run(10, 10, 10, 10).unwrap();
        broken[4] = None; // 512 unreadable in this run
        let e = export(&state, 1, &[Some(broken)]).unwrap();
        assert_eq!(e.series.vectorization_ratio, vec![None]);
        assert_eq!(e.series.scalar_fp, vec![Some(10)]);
    }

    #[test]
    fn export_reports_multiplexing_per_bucket() {
        let state = SimdState::mock(Scope::All);
        let mut r = run(100, 10, 10, 0).unwrap();
        r[3] = sc(500, 10, 5); // simd_256 scaled x2
        let e = export(&state, 1, &[Some(r)]).unwrap();
        assert_eq!(e.series.simd_256, Some(vec![Some(1000)]));
        assert_eq!(e.scaling.len(), 1);
        assert_eq!(
            e.scaling["simd_256"],
            SimdScaling {
                scaled_runs: 1,
                coverage: 0.5
            }
        );
        assert!(e.warning.unwrap().contains("simd_256 (50.0%)"));
    }

    #[test]
    fn export_is_none_when_unavailable() {
        let state = SimdState::unavailable(UnavailableReason::NoPmu);
        assert!(export(&state, 0, &[Some(vec![])]).is_none());
        assert!(export(&SimdState::mock(Scope::All), 0, &[]).is_none());
    }

    #[test]
    fn series_round_trips_through_json() {
        let state = SimdState::mock(Scope::All);
        let e = export(&state, 1, &[run(1, 2, 3, 0), run(2, 2, 4, 0)]).unwrap();
        let text = serde_json::to_string(&e.series).unwrap();
        let back: SimdSeries = serde_json::from_str(&text).unwrap();
        assert_eq!(back, e.series);
    }
}
