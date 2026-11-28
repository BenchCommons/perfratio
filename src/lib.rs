//! Measurement engine of `perfratio`.
//!
//! **Pre-stable.** Only the [`api`] module is meant for use by other crates,
//! and even it may change in any `0.x` release. All other modules are
//! implementation details of the command-line tool. They are public only so
//! that the binary can use them, are hidden from the documentation and carry
//! no compatibility promise.
//!
//! Use `perfratio = { version = "...", default-features = false }` to leave
//! out the command-line front end (`cli` feature: `clap`, `clap_complete`,
//! `indicatif`). The `perfratio` binary needs the default features.
//!
//! [`api::Benchmark`] runs a full benchmark (warmup, statistics, optional
//! energy and bootstrap intervals) without printing anything:
//!
//! ```
//! use perfratio::api::Benchmark;
//!
//! # fn main() -> anyhow::Result<()> {
//! # if cfg!(unix) {
//! let report = Benchmark::new(["true"]).runs(5).deep_stats(true).run()?;
//! println!("{:.6} s", report.result.mean);
//! # }
//! # Ok(())
//! # }
//! ```
//!
//! [`api::measure`] returns the raw per-run samples:
//!
//! ```
//! use perfratio::api::{measure, stats};
//!
//! # fn main() -> anyhow::Result<()> {
//! let m = measure(&["true"], 3)?;
//! assert_eq!(m.real.len(), 3);
//! println!("{:.6} s", stats::mean(&m.real));
//! # Ok(())
//! # }
//! ```
#![cfg_attr(
    all(windows, feature = "windows_process_extensions_main_thread_handle"),
    feature(windows_process_extensions_main_thread_handle)
)]
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod api;

#[doc(hidden)]
pub mod benchmark;
#[doc(hidden)]
pub mod budget;
pub mod calibration;
#[doc(hidden)]
pub mod cgroup;
#[cfg(feature = "cli")]
#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod command;
#[doc(hidden)]
pub mod compare;
#[doc(hidden)]
pub mod complexity;
#[doc(hidden)]
pub mod cooldown;
#[doc(hidden)]
pub mod counters;
#[doc(hidden)]
pub mod cpu_id;
#[doc(hidden)]
pub mod delta_table;
#[doc(hidden)]
pub mod efficiency;
#[doc(hidden)]
pub mod energy;
pub mod environment;
#[doc(hidden)]
pub mod environment_watch;
#[doc(hidden)]
pub mod error;
#[doc(hidden)]
pub mod evidence;
#[doc(hidden)]
pub mod export;
#[doc(hidden)]
pub mod frequency;
#[doc(hidden)]
pub mod import;
#[doc(hidden)]
pub mod mcp;
#[doc(hidden)]
pub mod metric;
#[doc(hidden)]
pub mod metric_registry;
pub mod optimize;
#[doc(hidden)]
pub mod options;
#[doc(hidden)]
pub mod outlier_detection;
#[doc(hidden)]
pub mod output;
#[doc(hidden)]
pub mod output_metric;
#[doc(hidden)]
pub mod parameter;
#[doc(hidden)]
pub mod pareto;
#[doc(hidden)]
pub mod perf_counters;
#[doc(hidden)]
pub mod pmu;
#[doc(hidden)]
pub mod pmu_events;
#[doc(hidden)]
pub mod pmu_scaling;
#[doc(hidden)]
pub mod quality;
#[doc(hidden)]
pub mod region;
/// Region-scoped CPU time, energy and counters.
pub mod region_scope;
#[doc(hidden)]
pub mod roofline;
#[doc(hidden)]
pub mod run_id;
#[doc(hidden)]
pub mod search;
#[doc(hidden)]
pub mod simd;
#[doc(hidden)]
pub mod stats;
#[doc(hidden)]
pub mod system_check;
#[doc(hidden)]
pub mod thermal;
#[doc(hidden)]
pub mod timer;
#[doc(hidden)]
pub mod topdown;
#[doc(hidden)]
pub mod util;
#[doc(hidden)]
pub mod validation;
#[doc(hidden)]
pub mod verdict;
