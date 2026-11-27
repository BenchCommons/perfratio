//! Cachegrind/Callgrind adapter (#383): `--collector cachegrind|callgrind`.
//! See `docs/specs/valgrind-collector.md`.
//!
//! The adapter runs the benchmarked command once more under `valgrind --tool=<tool>` and
//! exports the simulated event counts as `results[].collector_metrics` with `kind: simulated`.
//! The counts are deterministic, so one run is enough and it is not timed. It does not use the
//! plugin protocol of `crate::collector`: Valgrind must be the parent of the command, while a
//! plugin only observes a run that perfratio spawns. Both share the export (`collector_metrics`
//! and the `collector.<name>.<id>` descriptors).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{bail, ensure, Context, Result};

use crate::collector::{declare, Meta};
use crate::metric_registry::{Direction, Kind, Scope};

/// Names of the built-in collectors; plugins cannot use them.
pub const TOOL_NAMES: &[&str] = &["cachegrind", "callgrind"];

/// `true` if the exported key `<collector>.<id>` belongs to a built-in collector.
pub fn is_key(key: &str) -> bool {
    TOOL_NAMES
        .iter()
        .any(|tool| key.strip_prefix(tool).is_some_and(|r| r.starts_with('.')))
}

/// Variable that replaces `valgrind` as the program to run (tests, non-standard installs).
pub const BINARY_ENV: &str = "PERFRATIO_VALGRIND";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Cachegrind,
    Callgrind,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Cachegrind => "cachegrind",
            Tool::Callgrind => "callgrind",
        }
    }

    pub fn from_name(name: &str) -> Option<Tool> {
        match name {
            "cachegrind" => Some(Tool::Cachegrind),
            "callgrind" => Some(Tool::Callgrind),
            _ => None,
        }
    }
}

/// Event totals of one or more profile files.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Profile {
    pub totals: BTreeMap<String, u64>,
}

impl Profile {
    /// Adds the totals of another file (a child process).
    pub fn merge(&mut self, other: Profile) {
        for (event, count) in other.totals {
            let total = self.totals.entry(event).or_insert(0);
            *total = total.saturating_add(count);
        }
    }

    fn get(&self, event: &str) -> Option<u64> {
        self.totals.get(event).copied()
    }
}

/// Parses a Cachegrind or Callgrind output file: the `events:` header and the grand total
/// (`totals:` of Callgrind, else `summary:`). The per-function lines are not read.
pub fn parse_profile(text: &str) -> Result<Profile> {
    let mut events: Option<Vec<&str>> = None;
    let mut summary: Option<&str> = None;
    let mut totals: Option<&str> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("events:") {
            events = Some(rest.split_whitespace().collect());
        } else if let Some(rest) = line.strip_prefix("totals:") {
            totals = Some(rest);
        } else if let Some(rest) = line.strip_prefix("summary:") {
            summary = Some(rest);
        }
    }
    let events = events.context("the profile has no 'events:' line")?;
    let counts = totals
        .or(summary)
        .context("the profile has no 'summary:' or 'totals:' line")?;
    let counts: Vec<u64> = counts
        .split_whitespace()
        .map(|c| c.parse::<u64>())
        .collect::<std::result::Result<_, _>>()
        .context("the totals of the profile are not integers")?;
    ensure!(
        counts.len() == events.len(),
        "the profile lists {} events but {} totals",
        events.len(),
        counts.len()
    );
    Ok(Profile {
        totals: events.into_iter().map(str::to_string).zip(counts).collect(),
    })
}

/// Cycle estimate of iai and gungraun: L1 hits cost 1, last-level hits 5, RAM accesses 35.
fn estimated_cycles(profile: &Profile) -> Option<u64> {
    let memory = profile
        .get("Ir")?
        .saturating_add(profile.get("Dr")?)
        .saturating_add(profile.get("Dw")?);
    let l1_misses = profile
        .get("I1mr")?
        .saturating_add(profile.get("D1mr")?)
        .saturating_add(profile.get("D1mw")?);
    let ll_misses = profile
        .get("ILmr")?
        .saturating_add(profile.get("DLmr")?)
        .saturating_add(profile.get("DLmw")?);
    let l1_hits = memory.saturating_sub(l1_misses);
    let ll_hits = l1_misses.saturating_sub(ll_misses);
    Some(
        l1_hits
            .saturating_add(ll_hits.saturating_mul(5))
            .saturating_add(ll_misses.saturating_mul(35)),
    )
}

/// The exported metrics of a profile: the key is `<tool>.<id>`. Events the profile lacks (no
/// cache simulation) are left out, never written as 0.
pub fn metrics(tool: Tool, profile: &Profile) -> BTreeMap<String, f64> {
    let sum = |events: &[&str]| -> Option<u64> {
        events
            .iter()
            .map(|e| profile.get(e))
            .try_fold(0u64, |acc, v| v.map(|v| acc.saturating_add(v)))
    };
    let rows: [(&str, Option<u64>); 8] = [
        ("instructions", sum(&["Ir"])),
        ("l1_instruction_misses", sum(&["I1mr"])),
        ("ll_instruction_misses", sum(&["ILmr"])),
        ("data_reads", sum(&["Dr"])),
        ("data_writes", sum(&["Dw"])),
        ("l1_data_misses", sum(&["D1mr", "D1mw"])),
        ("ll_data_misses", sum(&["DLmr", "DLmw"])),
        ("estimated_cycles", estimated_cycles(profile)),
    ];
    rows.into_iter()
        .filter_map(|(id, value)| Some((format!("{}.{id}", tool.name()), value? as f64)))
        .collect()
}

/// Declares the descriptor data of the metrics of `tool` (`kind: simulated`, except the cycle
/// estimate, which is `estimated`).
pub fn declare_metrics(tool: Tool) {
    for id in [
        "instructions",
        "l1_instruction_misses",
        "ll_instruction_misses",
        "data_reads",
        "data_writes",
        "l1_data_misses",
        "ll_data_misses",
        "estimated_cycles",
    ] {
        declare(
            &format!("{}.{id}", tool.name()),
            Meta {
                unit: "count".into(),
                direction: Direction::LowerIsBetter,
                kind: if id == "estimated_cycles" {
                    Kind::Estimated
                } else {
                    Kind::Simulated
                },
                scope: Scope::ProcessTree,
                source: format!("valgrind.{}", tool.name()),
            },
        );
    }
}

fn binary() -> OsString {
    std::env::var_os(BINARY_ENV)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "valgrind".into())
}

/// Fails with a clear message if Valgrind cannot be run. Returns its version line.
pub fn check_available() -> Result<String> {
    let output = Command::new(binary())
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "'--collector' needs Valgrind, which could not be run ({e}). Install it \
                 (e.g. 'apt install valgrind'; Linux and some Unix systems only)"
            )
        })?;
    ensure!(
        output.status.success(),
        "'valgrind --version' failed with {}",
        output.status
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Result of the Valgrind run.
#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    /// `<tool>.<id>` to value
    pub metrics: BTreeMap<String, f64>,
}

/// The Valgrind command line that runs `command`: Valgrind options, `--`, program, arguments.
/// Child processes are followed (the default shell is the parent of the benchmarked program) and
/// every process writes its own file into `out_dir`.
pub fn wrap(tool: Tool, command: &Command, out_dir: &Path) -> Command {
    let mut wrapped = Command::new(binary());
    wrapped
        .arg(format!("--tool={}", tool.name()))
        .arg("--trace-children=yes")
        .arg("--cache-sim=yes")
        .arg(format!(
            "--log-file={}",
            out_dir.join("valgrind.log").display()
        ));
    let (flag, file) = match tool {
        Tool::Cachegrind => ("--cachegrind-out-file", "cachegrind.out.%p"),
        Tool::Callgrind => ("--callgrind-out-file", "callgrind.out.%p"),
    };
    let mut option = OsString::from(format!("{flag}="));
    option.push(out_dir.join(file));
    wrapped.arg(option).arg("--").arg(command.get_program());
    wrapped.args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => wrapped.env(key, value),
            None => wrapped.env_remove(key),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        wrapped.current_dir(dir);
    }
    // The run is not timed and its output is not part of the benchmark
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    wrapped
}

/// A copy of `command` (program, arguments, environment, working directory), made before the
/// command is spawned. The copy keeps no input or output redirection.
pub fn replica(command: &Command) -> Command {
    let mut copy = Command::new(command.get_program());
    copy.args(command.get_args());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => copy.env(key, value),
            None => copy.env_remove(key),
        };
    }
    if let Some(dir) = command.get_current_dir() {
        copy.current_dir(dir);
    }
    copy
}

fn profile_files(dir: &Path, tool: Tool) -> Result<Vec<PathBuf>> {
    let prefix = format!("{}.out.", tool.name());
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let is_profile = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(&prefix) && !n.ends_with(".tmp"));
        if is_profile {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// A temporary directory that only the user can read, removed on drop.
struct PrivateDir(PathBuf);

impl PrivateDir {
    fn create() -> std::io::Result<PrivateDir> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!(
            "perfratio-valgrind-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // `create` fails if the path exists, so a planted directory or link is never reused
        builder.create(&path)?;
        Ok(PrivateDir(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs `command` (already complete: arguments, environment) under `tool` and reads the
/// profiles of the processes it started.
pub fn measure(tool: Tool, command: &Command) -> Result<Outcome> {
    let dir = PrivateDir::create().context("could not create a temporary directory")?;
    let status = wrap(tool, command, dir.path())
        .status()
        .with_context(|| format!("could not run valgrind --tool={}", tool.name()))?;
    let files = profile_files(dir.path(), tool)?;
    if files.is_empty() {
        let log = std::fs::read_to_string(dir.path().join("valgrind.log")).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(5).collect();
        bail!(
            "valgrind --tool={} wrote no profile ({status}). Its log ends with:\n{}",
            tool.name(),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        );
    }
    let mut total = Profile::default();
    for file in files {
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("could not read {}", file.display()))?;
        total.merge(parse_profile(&text).with_context(|| format!("{}", file.display()))?);
    }
    Ok(Outcome {
        status,
        metrics: metrics(tool, &total),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const CACHEGRIND: &str = "\
desc: I1 cache:         32768 B, 64 B, 8-way associative
desc: Options:          --cache-sim=yes
cmd: ./prog
events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw
fl=main.c
fn=main
1 1000 2 1 400 10 5 200 4 2
summary: 1000 2 1 400 10 5 200 4 2
";

    const CALLGRIND_IR_ONLY: &str = "\
version: 1
creator: callgrind-3.22.0
events: Ir
fn=main
1 700
totals: 700
";

    #[test]
    fn parses_cachegrind_summary() {
        let profile = parse_profile(CACHEGRIND).unwrap();
        assert_eq!(profile.totals["Ir"], 1000);
        assert_eq!(profile.totals["DLmw"], 2);
        let m = metrics(Tool::Cachegrind, &profile);
        assert_eq!(m["cachegrind.instructions"], 1000.0);
        assert_eq!(m["cachegrind.l1_data_misses"], 14.0);
        assert_eq!(m["cachegrind.ll_data_misses"], 7.0);
        // memory 1600, L1 misses 16, LL misses 8: 1584 + 5 * 8 + 35 * 8
        assert_eq!(m["cachegrind.estimated_cycles"], 1904.0);
    }

    #[test]
    fn without_cache_simulation_only_instructions_are_exported() {
        let m = metrics(Tool::Callgrind, &parse_profile(CALLGRIND_IR_ONLY).unwrap());
        assert_eq!(m.len(), 1);
        assert_eq!(m["callgrind.instructions"], 700.0);
    }

    #[test]
    fn totals_of_child_processes_are_added() {
        let mut profile = parse_profile(CACHEGRIND).unwrap();
        profile.merge(parse_profile(CACHEGRIND).unwrap());
        assert_eq!(profile.totals["Ir"], 2000);
    }

    #[test]
    fn malformed_profiles_are_rejected() {
        for bad in [
            "",
            "events: Ir\n",
            "summary: 5\n",
            "events: Ir Dr\nsummary: 5\n",
            "events: Ir\nsummary: five\n",
        ] {
            assert!(parse_profile(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn wrapping_keeps_program_arguments_and_environment() {
        let mut command = Command::new("/bin/echo");
        command
            .arg("a b")
            .arg("$HOME")
            .env("X", "1")
            .env_remove("Y");
        let wrapped = wrap(Tool::Callgrind, &command, Path::new("/tmp/out"));
        let args: Vec<String> = wrapped
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let split = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(args[split + 1..], ["/bin/echo", "a b", "$HOME"]);
        assert!(args.contains(&"--tool=callgrind".to_string()));
        let expected = format!(
            "--callgrind-out-file={}",
            Path::new("/tmp/out").join("callgrind.out.%p").display()
        );
        assert!(args.contains(&expected));
        let envs: Vec<_> = wrapped.get_envs().collect();
        assert!(envs.contains(&(std::ffi::OsStr::new("X"), Some(std::ffi::OsStr::new("1")))));
        assert!(envs.contains(&(std::ffi::OsStr::new("Y"), None)));
    }
}
