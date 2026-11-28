//! `--roofline`: arithmetic intensity and placement against the roofline
//! (#379, `docs/specs/roofline.md`; Williams, Waterman and Patterson, CACM 2009).
//!
//! * FLOPs come from the `--simd` buckets (#211): retired scalar and packed
//!   floating-point arithmetic, times the lanes of the bucket for the chosen
//!   precision.
//! * Memory traffic is `cache-misses` (last-level cache misses) times the cache
//!   line size. This is the documented fallback while the bandwidth counters of
//!   #378 are absent: it misses hardware prefetches and write-backs on most
//!   CPUs, so it is a lower bound of the traffic and an upper bound of the
//!   arithmetic intensity.
//! * Peaks are either configured (`--roofline-peaks GFLOPS,GBPS`) or the best
//!   values observed over the benchmarked commands, which are lower bounds of
//!   the real ceilings.
//!
//! Everything here is arithmetic over per-run numbers, so it is tested with
//! synthetic values on every platform.

use crate::benchmark::benchmark_result::BenchmarkResult;
use crate::simd::{Bucket, SimdSeries};
use serde::Serialize;
use std::collections::BTreeMap;

/// Bytes moved per last-level cache miss.
pub const CACHE_LINE_BYTES: f64 = 64.0;

/// Where the bytes of the arithmetic intensity come from (`bytes_source`).
pub const BYTES_SOURCE: &str = "llc_misses";

/// Element width the packed FP counts are multiplied with. `--simd` does not
/// split single and double precision, so the user says which one the kernel uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Precision {
    Single,
    #[default]
    Double,
}

impl Precision {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "single" => Some(Self::Single),
            "double" => Some(Self::Double),
            _ => None,
        }
    }

    fn element_bits(self) -> f64 {
        match self {
            Self::Single => 32.0,
            Self::Double => 64.0,
        }
    }

    /// FLOPs per retired count of a bucket: 1 for scalar, `width / element`
    /// for packed. Intel counts an FMA twice, which makes it two FLOPs per lane.
    pub fn flops_per_count(self, bucket: Bucket) -> f64 {
        let width = match bucket {
            Bucket::ScalarFp => return 1.0,
            Bucket::Simd128 => 128.0,
            Bucket::Simd256 => 256.0,
            Bucket::Simd512 => 512.0,
        };
        width / self.element_bits()
    }
}

include!("roofline_peaks.rs");

/// The roofs of `--roofline-peaks`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PeaksSetting {
    /// The best GFLOP/s and GB/s over the benchmarked commands
    Observed,
    Configured {
        gflops: f64,
        gbytes_per_s: f64,
    },
}

impl PeaksSetting {
    /// `observed` or `GFLOPS,GBPS` (both finite and above zero).
    pub fn parse(value: &str) -> Result<Self, String> {
        Ok(match parse_roofline_peaks(value)? {
            None => Self::Observed,
            Some((gflops, gbytes_per_s)) => Self::Configured {
                gflops,
                gbytes_per_s,
            },
        })
    }
}

/// The resolved `--roofline` request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub peaks: PeaksSetting,
    pub precision: Precision,
}

/// One run: FLOPs, bytes and wall time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    pub flops: f64,
    pub bytes: f64,
    pub seconds: f64,
}

/// Everything the analysis needs of one command. `runs` has one entry per run,
/// `None` where a counter or the time could not be read.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub command: String,
    pub parameters: BTreeMap<String, String>,
    pub runs: Vec<Option<Run>>,
    /// Why no run can be built (`--simd` or `cache-misses` unavailable)
    pub missing: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Peaks {
    pub gflops: f64,
    pub gbytes_per_s: f64,
    /// `configured` or `observed`
    pub source: &'static str,
}

/// Placement of one command (`roofline.results[]`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Placement {
    pub command: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
    /// `ok`, `no_flops` (no floating-point arithmetic retired), `no_traffic`
    /// (no cache miss counted: the intensity is unbounded) or `unavailable`
    pub status: &'static str,
    /// Why the status is `unavailable`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub runs_used: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flops_per_run: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_per_run: Option<f64>,
    /// FLOP per byte
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arithmetic_intensity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gflops: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gbytes_per_s: Option<f64>,
    /// `memory` below the ridge point, `compute` at or above it; needs peaks
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound: Option<&'static str>,
    /// `min(peak GFLOP/s, intensity × peak GB/s)`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attainable_gflops: Option<f64>,
    /// `gflops / attainable_gflops`; above 1 when the peaks are too low or
    /// the miss count undercounts the traffic
    #[serde(skip_serializing_if = "Option::is_none")]
    pub efficiency: Option<f64>,
}

/// The `roofline` object of the JSON export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Roofline {
    pub precision: Precision,
    pub bytes_source: &'static str,
    pub cache_line_bytes: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peaks: Option<Peaks>,
    /// `peaks.gflops / peaks.gbytes_per_s`, in FLOP per byte
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ridge_point: Option<f64>,
    pub results: Vec<Placement>,
    /// Why `peaks` are missing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peaks_note: Option<&'static str>,
}

/// FLOPs of every run of a `--simd` series. `None` where a present bucket
/// could not be read.
pub fn flops_per_run(series: &SimdSeries, precision: Precision) -> Vec<Option<f64>> {
    let buckets = [
        Bucket::ScalarFp,
        Bucket::Simd128,
        Bucket::Simd256,
        Bucket::Simd512,
    ];
    (0..series.scalar_fp.len())
        .map(|i| {
            buckets
                .iter()
                .filter_map(|&b| series.bucket(b).map(|v| (b, v)))
                .try_fold(0.0, |sum, (b, values)| {
                    let count = (*values.get(i)?)?;
                    Some(sum + count as f64 * precision.flops_per_count(b))
                })
        })
        .collect()
}

/// Bytes of every run from the `cache-misses` counter (last-level misses ×
/// line size). Without a key at the default scope, a user + kernel pair is
/// summed.
fn bytes_per_run(counters: &BTreeMap<String, Vec<Option<u64>>>, runs: usize) -> Vec<Option<f64>> {
    let at = |key: &str| counters.get(key);
    let misses: Vec<Option<u64>> = match (
        at("cache-misses"),
        at("cache-misses:u"),
        at("cache-misses:k"),
    ) {
        (Some(all), _, _) => all.clone(),
        (None, Some(u), Some(k)) => u.iter().zip(k).map(|(u, k)| Some((*u)? + (*k)?)).collect(),
        _ => return vec![None; runs],
    };
    (0..runs)
        .map(|i| {
            misses
                .get(i)
                .copied()
                .flatten()
                .map(|m| m as f64 * CACHE_LINE_BYTES)
        })
        .collect()
}

impl Input {
    /// Builds the input of one measured result.
    pub fn from_result(result: &BenchmarkResult, precision: Precision) -> Self {
        let mut input = Self {
            command: result.command.clone(),
            parameters: result.parameters.clone(),
            runs: Vec::new(),
            missing: None,
        };
        let Some(series) = &result.simd else {
            input.missing = Some("simd_unavailable");
            return input;
        };
        let Some(times) = &result.times else {
            input.missing = Some("no_run_times");
            return input;
        };
        let Some(counters) = &result.counters else {
            input.missing = Some("cache_misses_unavailable");
            return input;
        };
        let bytes = bytes_per_run(counters, times.len());
        if bytes.iter().all(Option::is_none) {
            input.missing = Some("cache_misses_unavailable");
            return input;
        }
        let flops = flops_per_run(series, precision);
        input.runs = (0..times.len())
            .map(|i| {
                Some(Run {
                    flops: (*flops.get(i)?)?,
                    bytes: bytes[i]?,
                    seconds: times[i],
                })
            })
            .collect();
        input
    }
}

fn unavailable(input: &Input, reason: &'static str) -> Placement {
    Placement {
        command: input.command.clone(),
        parameters: input.parameters.clone(),
        status: "unavailable",
        reason: Some(reason),
        runs_used: 0,
        flops_per_run: None,
        bytes_per_run: None,
        arithmetic_intensity: None,
        gflops: None,
        gbytes_per_s: None,
        bound: None,
        attainable_gflops: None,
        efficiency: None,
    }
}

/// Totals over the runs that have all three numbers and a positive time.
fn measure(input: &Input) -> Placement {
    if let Some(reason) = input.missing {
        return unavailable(input, reason);
    }
    let usable: Vec<Run> = input
        .runs
        .iter()
        .flatten()
        .copied()
        .filter(|r| r.seconds > 0.0)
        .collect();
    if usable.is_empty() {
        return unavailable(input, "no_complete_run");
    }
    let n = usable.len() as f64;
    let flops: f64 = usable.iter().map(|r| r.flops).sum();
    let bytes: f64 = usable.iter().map(|r| r.bytes).sum();
    let seconds: f64 = usable.iter().map(|r| r.seconds).sum();
    let status = if flops == 0.0 {
        "no_flops"
    } else if bytes == 0.0 {
        "no_traffic"
    } else {
        "ok"
    };
    Placement {
        status,
        reason: None,
        runs_used: usable.len(),
        flops_per_run: Some(flops / n),
        bytes_per_run: Some(bytes / n),
        arithmetic_intensity: (status == "ok").then(|| flops / bytes),
        gflops: Some(flops / seconds / 1e9),
        gbytes_per_s: Some(bytes / seconds / 1e9),
        ..unavailable(input, "")
    }
}

fn place(p: &mut Placement, peaks: &Peaks) {
    let (Some(ai), Some(gflops)) = (p.arithmetic_intensity, p.gflops) else {
        return;
    };
    let ridge = peaks.gflops / peaks.gbytes_per_s;
    let attainable = peaks.gflops.min(ai * peaks.gbytes_per_s);
    p.bound = Some(if ai < ridge { "memory" } else { "compute" });
    p.attainable_gflops = Some(attainable);
    p.efficiency = (attainable > 0.0).then(|| gflops / attainable);
}

/// The roofline of all commands.
pub fn analyze(config: &Config, inputs: &[Input]) -> Roofline {
    let mut results: Vec<Placement> = inputs.iter().map(measure).collect();
    let (peaks, peaks_note) = match config.peaks {
        PeaksSetting::Configured {
            gflops,
            gbytes_per_s,
        } => (
            Some(Peaks {
                gflops,
                gbytes_per_s,
                source: "configured",
            }),
            None,
        ),
        PeaksSetting::Observed => {
            let ok: Vec<&Placement> = results.iter().filter(|p| p.status == "ok").collect();
            let best = |f: fn(&Placement) -> Option<f64>| {
                ok.iter().filter_map(|p| f(p)).fold(0.0, f64::max)
            };
            let (gflops, gbytes_per_s) = (best(|p| p.gflops), best(|p| p.gbytes_per_s));
            if ok.len() < 2 || gflops <= 0.0 || gbytes_per_s <= 0.0 {
                (
                    None,
                    Some("observed peaks need two commands with a measurable intensity; give --roofline-peaks GFLOPS,GBPS"),
                )
            } else {
                (
                    Some(Peaks {
                        gflops,
                        gbytes_per_s,
                        source: "observed",
                    }),
                    None,
                )
            }
        }
    };
    if let Some(peaks) = &peaks {
        for p in &mut results {
            place(p, peaks);
        }
    }
    Roofline {
        precision: config.precision,
        bytes_source: BYTES_SOURCE,
        cache_line_bytes: CACHE_LINE_BYTES,
        ridge_point: peaks.map(|p| p.gflops / p.gbytes_per_s),
        peaks,
        results,
        peaks_note,
    }
}

fn si(value: f64) -> String {
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// Terminal block.
pub fn terminal(roofline: &Roofline) -> String {
    let mut out = format!(
        "\nRoofline ({} precision; memory traffic = last-level cache misses × {} B, a lower bound):\n",
        match roofline.precision {
            Precision::Single => "single",
            Precision::Double => "double",
        },
        CACHE_LINE_BYTES
    );
    for p in &roofline.results {
        let rates = |p: &Placement| match (p.gflops, p.gbytes_per_s) {
            (Some(f), Some(b)) => format!("{} GFLOP/s · {} GB/s", si(f), si(b)),
            _ => String::new(),
        };
        let line = match p.status {
            "ok" => {
                let ai = p.arithmetic_intensity.unwrap_or_default();
                let mut s = format!("AI {} FLOP/B · {}", si(ai), rates(p));
                if let (Some(bound), Some(eff), Some(att)) =
                    (p.bound, p.efficiency, p.attainable_gflops)
                {
                    s.push_str(&format!(
                        " · {bound}-bound, {:.0}% of the attainable {} GFLOP/s",
                        eff * 100.0,
                        si(att)
                    ));
                }
                s
            }
            "no_flops" => {
                "no floating-point arithmetic retired (integer SIMD is not counted)".to_string()
            }
            "no_traffic" => format!(
                "no cache misses counted, the intensity is unbounded · {}",
                rates(p)
            ),
            _ => format!("unavailable ({})", p.reason.unwrap_or("unknown")),
        };
        out.push_str(&format!("  {}: {line}\n", p.command));
    }
    match (&roofline.peaks, roofline.ridge_point) {
        (Some(peaks), Some(ridge)) => out.push_str(&format!(
            "  Peaks ({}): {} GFLOP/s, {} GB/s; ridge point {} FLOP/B\n",
            peaks.source,
            si(peaks.gflops),
            si(peaks.gbytes_per_s),
            si(ridge)
        )),
        _ => {
            if let Some(note) = roofline.peaks_note {
                out.push_str(&format!("  No placement: {note}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(flops: f64, bytes: f64, seconds: f64) -> Option<Run> {
        Some(Run {
            flops,
            bytes,
            seconds,
        })
    }

    fn input(name: &str, runs: Vec<Option<Run>>) -> Input {
        Input {
            command: name.to_string(),
            parameters: BTreeMap::new(),
            runs,
            missing: None,
        }
    }

    fn configured(gflops: f64, gbytes_per_s: f64) -> Config {
        Config {
            peaks: PeaksSetting::Configured {
                gflops,
                gbytes_per_s,
            },
            precision: Precision::Double,
        }
    }

    #[test]
    fn lanes_follow_precision() {
        assert_eq!(Precision::Double.flops_per_count(Bucket::ScalarFp), 1.0);
        assert_eq!(Precision::Double.flops_per_count(Bucket::Simd128), 2.0);
        assert_eq!(Precision::Double.flops_per_count(Bucket::Simd256), 4.0);
        assert_eq!(Precision::Double.flops_per_count(Bucket::Simd512), 8.0);
        assert_eq!(Precision::Single.flops_per_count(Bucket::Simd256), 8.0);
        assert_eq!(Precision::Single.flops_per_count(Bucket::Simd512), 16.0);
    }

    #[test]
    fn flops_from_simd_buckets() {
        let series = SimdSeries {
            scalar_fp: vec![Some(10), Some(10), None],
            simd_128: Some(vec![Some(5), None, Some(1)]),
            simd_256: Some(vec![Some(2), Some(2), Some(1)]),
            ..Default::default()
        };
        let double = flops_per_run(&series, Precision::Double);
        // 10 + 5*2 + 2*4; run 1 has an unreadable 128-bit bucket; run 2 no scalar
        assert_eq!(double, vec![Some(28.0), None, None]);
        assert_eq!(
            flops_per_run(&series, Precision::Single)[0],
            Some(10.0 + 20.0 + 16.0)
        );
    }

    #[test]
    fn intensity_and_rates_from_totals() {
        // 8 GFLOP and 2 GB in 2 s, twice
        let i = input("k", vec![run(8e9, 2e9, 2.0), run(8e9, 2e9, 2.0), None]);
        let r = analyze(&configured(100.0, 10.0), &[i]);
        let p = &r.results[0];
        assert_eq!(p.status, "ok");
        assert_eq!(p.runs_used, 2);
        assert_eq!(p.arithmetic_intensity, Some(4.0));
        assert_eq!(p.gflops, Some(4.0));
        assert_eq!(p.gbytes_per_s, Some(1.0));
        assert_eq!(p.flops_per_run, Some(8e9));
    }

    #[test]
    fn memory_bound_below_the_ridge() {
        // peaks 100 GFLOP/s, 10 GB/s: ridge 10 FLOP/B. AI 4 is memory-bound,
        // attainable 40 GFLOP/s, measured 4 GFLOP/s: 10 %.
        let r = analyze(
            &configured(100.0, 10.0),
            &[input("k", vec![run(8e9, 2e9, 2.0)])],
        );
        let p = &r.results[0];
        assert_eq!(r.ridge_point, Some(10.0));
        assert_eq!(p.bound, Some("memory"));
        assert_eq!(p.attainable_gflops, Some(40.0));
        assert!((p.efficiency.unwrap() - 0.1).abs() < 1e-12);
        assert_eq!(r.peaks.unwrap().source, "configured");
    }

    #[test]
    fn compute_bound_at_or_above_the_ridge() {
        // AI 20 > ridge 10: attainable is the compute peak
        let r = analyze(
            &configured(100.0, 10.0),
            &[input("k", vec![run(40e9, 2e9, 1.0)])],
        );
        let p = &r.results[0];
        assert_eq!(p.bound, Some("compute"));
        assert_eq!(p.attainable_gflops, Some(100.0));
        assert!((p.efficiency.unwrap() - 0.4).abs() < 1e-12);
        // exactly on the ridge counts as compute-bound
        let r = analyze(
            &configured(100.0, 10.0),
            &[input("k", vec![run(20e9, 2e9, 1.0)])],
        );
        assert_eq!(r.results[0].bound, Some("compute"));
    }

    #[test]
    fn observed_peaks_are_the_best_of_the_commands() {
        let slow = input("slow", vec![run(2e9, 2e9, 1.0)]); // 2 GFLOP/s, 2 GB/s
        let fast = input("fast", vec![run(30e9, 3e9, 1.0)]); // 30 GFLOP/s, 3 GB/s
        let config = Config {
            peaks: PeaksSetting::Observed,
            precision: Precision::Double,
        };
        let r = analyze(&config, &[slow, fast]);
        let peaks = r.peaks.unwrap();
        assert_eq!(
            (peaks.gflops, peaks.gbytes_per_s, peaks.source),
            (30.0, 3.0, "observed")
        );
        assert_eq!(r.ridge_point, Some(10.0));
        // the fast one defines the compute peak and sits on it
        assert!((r.results[1].efficiency.unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(r.results[0].bound, Some("memory"));
    }

    #[test]
    fn observed_peaks_need_two_commands() {
        let config = Config {
            peaks: PeaksSetting::Observed,
            precision: Precision::Double,
        };
        let r = analyze(&config, &[input("only", vec![run(1e9, 1e9, 1.0)])]);
        assert!(r.peaks.is_none() && r.ridge_point.is_none());
        assert!(r.peaks_note.unwrap().contains("--roofline-peaks"));
        assert_eq!(r.results[0].bound, None);
        assert_eq!(r.results[0].arithmetic_intensity, Some(1.0));
    }

    #[test]
    fn degenerate_inputs_have_a_status_and_no_nan() {
        let none = input("no flops", vec![run(0.0, 64.0, 1.0)]);
        let nobytes = input("no bytes", vec![run(1e9, 0.0, 1.0)]);
        let nothing = input("nothing", vec![None, run(1.0, 1.0, 0.0)]);
        let mut gone = input("gone", vec![]);
        gone.missing = Some("simd_unavailable");
        let r = analyze(&configured(10.0, 1.0), &[none, nobytes, nothing, gone]);
        let status: Vec<&str> = r.results.iter().map(|p| p.status).collect();
        assert_eq!(
            status,
            ["no_flops", "no_traffic", "unavailable", "unavailable"]
        );
        assert_eq!(r.results[1].arithmetic_intensity, None);
        assert_eq!(r.results[1].gflops, Some(1.0));
        assert_eq!(r.results[2].reason, Some("no_complete_run"));
        assert_eq!(r.results[3].reason, Some("simd_unavailable"));
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("null") && !json.contains("NaN"), "{json}");
    }

    #[test]
    fn peaks_setting_parses() {
        assert_eq!(PeaksSetting::parse("observed"), Ok(PeaksSetting::Observed));
        assert_eq!(
            PeaksSetting::parse("48, 20.5"),
            Ok(PeaksSetting::Configured {
                gflops: 48.0,
                gbytes_per_s: 20.5
            })
        );
        for bad in ["48", "0,10", "10,0", "a,b", "-1,2", "inf,1", "1,2,3", ""] {
            assert!(PeaksSetting::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bytes_come_from_cache_misses() {
        let mut counters = BTreeMap::new();
        counters.insert("cache-misses".to_string(), vec![Some(10), None]);
        assert_eq!(bytes_per_run(&counters, 2), [Some(640.0), None]);
        let mut split = BTreeMap::new();
        split.insert("cache-misses:u".to_string(), vec![Some(1)]);
        split.insert("cache-misses:k".to_string(), vec![Some(2)]);
        assert_eq!(bytes_per_run(&split, 1), [Some(192.0)]);
        assert_eq!(bytes_per_run(&BTreeMap::new(), 2), [None, None]);
    }

    #[test]
    fn built_from_a_result() {
        let mut result = BenchmarkResult {
            command: "k".to_string(),
            times: Some(vec![2.0, 2.0]),
            simd: Some(SimdSeries {
                scalar_fp: vec![Some(1_000_000_000); 2],
                simd_256: Some(vec![Some(1_000_000_000); 2]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            Input::from_result(&result, Precision::Double).missing,
            Some("cache_misses_unavailable")
        );
        let mut counters = BTreeMap::new();
        // 100M misses × 64 B = 6.4 GB per run
        counters.insert("cache-misses".to_string(), vec![Some(100_000_000); 2]);
        result.counters = Some(counters);
        let input = Input::from_result(&result, Precision::Double);
        let r = analyze(&configured(10.0, 10.0), &[input]);
        let p = &r.results[0];
        // 1e9 + 4e9 FLOPs / 6.4e9 B
        assert!((p.arithmetic_intensity.unwrap() - 5.0 / 6.4).abs() < 1e-12);
        assert_eq!(p.gflops, Some(2.5));
        result.simd = None;
        assert_eq!(
            Input::from_result(&result, Precision::Double).missing,
            Some("simd_unavailable")
        );
    }

    #[test]
    fn terminal_block_names_bound_and_peaks() {
        let r = analyze(
            &configured(100.0, 10.0),
            &[input("./k", vec![run(8e9, 2e9, 2.0)])],
        );
        let text = terminal(&r);
        assert!(
            text.contains("AI 4.00 FLOP/B · 4.00 GFLOP/s · 1.00 GB/s"),
            "{text}"
        );
        assert!(
            text.contains("memory-bound, 10% of the attainable 40.0 GFLOP/s"),
            "{text}"
        );
        assert!(
            text.contains("Peaks (configured): 100 GFLOP/s, 10.0 GB/s; ridge point 10.0 FLOP/B"),
            "{text}"
        );
        assert!(text.contains("lower bound"), "{text}");
    }
}
