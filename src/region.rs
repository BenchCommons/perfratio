//! Region timing protocol (`--region`), see `docs/specs/region-protocol.md`.
//!
//! The benchmarked program brackets the code of interest with `start`/`stop`
//! calls of a small client (`sdk/`). The client appends one text line per
//! event to the file named by `PERFRATIO_REGION_FILE`; perfratio reads the file
//! after the run and sums the intervals per region name.
//!
//! Wire format, protocol 1 (UTF-8, one event per line, `\n` or `\r\n`):
//!
//! ```text
//! hello 1 [<sdk-id>]
//! start <name> <t_ns>
//! stop <name> <t_ns>
//! ```
//!
//! `t_ns` is a monotonic clock reading in nanoseconds. Only differences within
//! one file are used.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, ensure, Context, Result};

use crate::util::sanitize::escape_control_chars;

/// The protocol version this perfratio speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// Environment variable with the path of the event file.
pub const ENV_FILE: &str = "PERFRATIO_REGION_FILE";
/// Environment variable with the protocol version perfratio expects.
pub const ENV_PROTOCOL: &str = "PERFRATIO_REGION_PROTOCOL";
/// Name of the region measured by a bare `--region`.
pub const DEFAULT_REGION: &str = "default";
/// Prefix of the selector `--metric region:<name>`.
pub const METRIC_PREFIX: &str = "region:";
/// Key prefix of the per-run start/stop pair count inside the runner.
pub const COUNT_PREFIX: &str = "region_count:";
/// Longest accepted event file, so a runaway program cannot exhaust memory.
const MAX_FILE_BYTES: u64 = 64 << 20;

/// Whether `name` is a valid region name: ASCII letters, digits and `_`.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Sum of the intervals of one region in one run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Totals {
    pub seconds: f64,
    /// Number of start/stop pairs.
    pub count: u64,
}

/// Everything one run reported.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// Identifier the client announced in its `hello` line, if any.
    pub sdk: Option<String>,
    pub regions: BTreeMap<String, Totals>,
}

/// An SDK identifier such as `c/1.0`: ASCII letters, digits and `_-./+`, at
/// most 64 characters.
fn is_valid_sdk_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./+".contains(c))
}

/// Longest piece of program-provided text echoed in an error message.
const MAX_ECHO: usize = 64;

/// Escape control characters (CWE-150) and cut text from the measured program
/// to [`MAX_ECHO`] characters, for use in error messages.
fn shorten(s: &str) -> String {
    let escaped = escape_control_chars(s);
    let mut it = escaped.chars();
    let head: String = it.by_ref().take(MAX_ECHO).collect();
    if it.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

/// Parse the event file of one run.
///
/// Returns `Ok(None)` for an empty file: the program never said hello, so it
/// is not instrumented (or it exited before writing).
pub fn parse(text: &str) -> Result<Option<Report>> {
    let mut report = Report::default();
    let mut said_hello = false;
    // name -> start time of the interval that is open
    let mut open: HashMap<&str, u64> = HashMap::new();
    let mut totals: BTreeMap<&str, (u128, u64)> = BTreeMap::new();

    for (index, raw) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let kind = parts.next().unwrap_or_default();
        if kind == "hello" {
            ensure!(
                !said_hello,
                "region protocol: line {line_no}: duplicate 'hello'"
            );
            let version = parts
                .next()
                .and_then(|v| v.parse::<u32>().ok())
                .ok_or_else(|| {
                    anyhow!("region protocol: line {line_no}: 'hello' needs a protocol version")
                })?;
            if version != PROTOCOL_VERSION {
                bail!(
                    "region protocol mismatch: the program speaks protocol {version} but this \
                     perfratio speaks protocol {PROTOCOL_VERSION}. Use an SDK from the same \
                     perfratio release."
                );
            }
            report.sdk = match parts.next() {
                None => None,
                Some(id) => {
                    ensure!(
                        is_valid_sdk_id(id),
                        "region protocol: line {line_no}: invalid SDK identifier '{}' \
                         (ASCII letters, digits and '_-./+', at most 64 characters)",
                        shorten(id)
                    );
                    Some(id.to_string())
                }
            };
            said_hello = true;
            continue;
        }
        ensure!(
            said_hello,
            "region protocol: line {line_no}: expected 'hello {PROTOCOL_VERSION}' first, got '{}'",
            shorten(kind)
        );
        let name = parts
            .next()
            .ok_or_else(|| anyhow!("region protocol: line {line_no}: '{kind}' needs a name"))?;
        ensure!(
            is_valid_name(name),
            "region protocol: line {line_no}: invalid region name '{}' \
             (ASCII letters, digits and '_', at most 64 characters)",
            shorten(name)
        );
        let t: u64 = parts.next().and_then(|t| t.parse().ok()).ok_or_else(|| {
            anyhow!(
                "region protocol: line {line_no}: '{} {name}' needs a time in nanoseconds",
                shorten(kind)
            )
        })?;
        match kind {
            "start" => {
                ensure!(
                    open.insert(name, t).is_none(),
                    "region '{name}' was started twice without a stop (line {line_no}); \
                     regions of the same name must not overlap"
                );
            }
            "stop" => {
                let start = open.remove(name).ok_or_else(|| {
                    anyhow!("region '{name}' was stopped without a start (line {line_no})")
                })?;
                ensure!(
                    t >= start,
                    "region '{name}' stops before it starts (line {line_no}); \
                     the client must use a monotonic clock"
                );
                let entry = totals.entry(name).or_insert((0, 0));
                entry.0 += u128::from(t - start);
                entry.1 += 1;
            }
            other => bail!(
                "region protocol: line {line_no}: unknown event '{}'",
                shorten(other)
            ),
        }
    }

    if !said_hello {
        return Ok(None);
    }
    if let Some(name) = open.keys().min() {
        bail!(
            "region '{name}' was started but never stopped. Stop it before the program exits \
             (and call flush() before exiting abnormally)."
        );
    }
    report.regions = totals
        .into_iter()
        .map(|(name, (ns, count))| {
            (
                name.to_string(),
                Totals {
                    seconds: ns as f64 / 1e9,
                    count,
                },
            )
        })
        .collect();
    Ok(Some(report))
}

/// The event file of one run. It is created private (mode 0600 on Unix) and
/// removed when the value is dropped.
#[derive(Debug)]
pub struct RunFile {
    path: PathBuf,
}

impl RunFile {
    pub fn create() -> io::Result<Self> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir();
        loop {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("perfratio-region-{}-{n}.log", std::process::id()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(_) => return Ok(RunFile { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Set the protocol environment variables on `command`.
    pub fn apply(&self, command: &mut std::process::Command) {
        command
            .env(ENV_FILE, &self.path)
            .env(ENV_PROTOCOL, PROTOCOL_VERSION.to_string());
    }

    /// Read and parse what the run wrote.
    pub fn read(&self) -> Result<Option<Report>> {
        let file = File::open(&self.path)
            .with_context(|| format!("could not read the region file {}", self.path.display()))?;
        let mut text = String::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_string(&mut text)
            .context("the region file is not valid UTF-8 text")?;
        ensure!(
            text.len() as u64 <= MAX_FILE_BYTES,
            "the region file exceeds {} MiB; are regions started in a tight loop?",
            MAX_FILE_BYTES >> 20
        );
        parse(&text)
    }
}

impl Drop for RunFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Region names that `--metric region:<name>` selects, or `None`.
pub fn metric_region(metric_name: &str) -> Option<&str> {
    metric_name.strip_prefix(METRIC_PREFIX)
}

/// Key under which the runner stores the total time of region `name`.
pub fn time_key(name: &str) -> String {
    format!("{METRIC_PREFIX}{name}")
}

/// Key under which the runner stores the pair count of region `name`.
pub fn count_key(name: &str) -> String {
    format!("{COUNT_PREFIX}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_limits_program_text_in_errors() {
        let e = parse("hello 1\nstart a 1\n\u{1b}[2Jx a 1\n")
            .unwrap_err()
            .to_string();
        assert!(!e.contains('\u{1b}'), "{e}");
        assert!(e.contains("\\u{1b}"), "{e}");
        let long = "z".repeat(500);
        let e = parse(&format!("hello 1\n{long} a 1\n"))
            .unwrap_err()
            .to_string();
        assert!(e.len() < 250 && e.contains("..."), "{e}");
    }

    #[test]
    fn rejects_bad_sdk_ids() {
        assert!(parse("hello 1 \u{1b}[2J\n").is_err());
        assert!(parse(&format!("hello 1 {}\n", "a".repeat(65))).is_err());
        assert!(parse("hello 1 rust/1.2+x\n").is_ok());
    }

    fn total(report: &Report, name: &str) -> Totals {
        report.regions[name]
    }

    #[test]
    fn parses_pairs_and_sums_repeats() {
        let text = "hello 1 c/1.0\nstart a 1000\nstop a 3000\nstart a 10000\nstop a 10500\n";
        let report = parse(text).unwrap().unwrap();
        assert_eq!(report.sdk.as_deref(), Some("c/1.0"));
        let a = total(&report, "a");
        assert_eq!(a.count, 2);
        assert!((a.seconds - 2.5e-6).abs() < 1e-15);
    }

    #[test]
    fn nested_and_overlapping_different_names_are_allowed() {
        let text = "hello 1\nstart outer 0\nstart inner 10\nstop outer 50\nstop inner 100\n";
        let report = parse(text).unwrap().unwrap();
        assert_eq!(report.sdk, None);
        assert_eq!(total(&report, "outer").seconds, 50e-9);
        assert_eq!(total(&report, "inner").seconds, 90e-9);
    }

    #[test]
    fn empty_file_is_not_instrumented() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("\n# comment\n").unwrap(), None);
    }

    #[test]
    fn hello_without_regions_has_no_regions() {
        let report = parse("hello 1\r\n").unwrap().unwrap();
        assert!(report.regions.is_empty());
    }

    #[test]
    fn version_mismatch_is_reported() {
        let error = parse("hello 2 x\n").unwrap_err().to_string();
        assert!(error.contains("protocol mismatch"), "{error}");
        assert!(error.contains("protocol 2"), "{error}");
    }

    #[test]
    fn protocol_errors() {
        for (text, needle) in [
            ("start a 1\n", "expected 'hello 1' first"),
            ("hello 1\nstart a 1\nstart a 2\n", "started twice"),
            ("hello 1\nstop a 2\n", "without a start"),
            ("hello 1\nstart a 5\nstop a 1\n", "monotonic"),
            ("hello 1\nstart a 5\n", "never stopped"),
            ("hello 1\nstart a\n", "needs a time"),
            ("hello 1\nstart a x\n", "needs a time"),
            ("hello 1\nstart a-b 1\n", "invalid region name"),
            ("hello 1\nhello 1\n", "duplicate"),
            ("hello 1\nbogus a 1\n", "unknown event"),
            ("hello\n", "protocol version"),
        ] {
            let error = parse(text).unwrap_err().to_string();
            assert!(error.contains(needle), "{text:?}: {error}");
        }
    }

    /// Small deterministic generator, so the test needs no extra dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    /// Pairing and summation hold for random interleavings of intervals of
    /// several names (nesting and overlap across names included).
    #[test]
    fn random_event_orders_pair_and_sum_correctly() {
        let names = ["a", "b", "c", "d"];
        for seed in 0..200u64 {
            let mut rng = Lcg(seed);
            // intervals per name: non-overlapping within a name
            let mut intervals: Vec<(usize, u64, u64)> = Vec::new();
            let mut expected = [(0u128, 0u64); 4];
            for (n, expected) in expected.iter_mut().enumerate() {
                let mut t = rng.next() % 100;
                for _ in 0..(rng.next() % 6) {
                    let len = 1 + rng.next() % 1000;
                    intervals.push((n, t, t + len));
                    expected.0 += u128::from(len);
                    expected.1 += 1;
                    t += len + rng.next() % 50;
                }
            }
            // events sorted by time; at equal times a stop precedes a start of
            // the same name only if it belongs to an earlier interval
            let mut events: Vec<(u64, u8, usize)> = Vec::new();
            for &(n, s, e) in &intervals {
                events.push((s, 1, n));
                events.push((e, 0, n));
            }
            events.sort_by_key(|&(t, kind, n)| (t, kind, n));
            let mut text = String::from("hello 1\n");
            for (t, kind, n) in events {
                let verb = if kind == 1 { "start" } else { "stop" };
                text.push_str(&format!("{verb} {} {t}\n", names[n]));
            }
            let report = parse(&text).unwrap().unwrap();
            for (n, name) in names.iter().enumerate() {
                match report.regions.get(*name) {
                    Some(t) => {
                        assert_eq!(t.count, expected[n].1, "seed {seed} {name}");
                        assert!(
                            (t.seconds - expected[n].0 as f64 / 1e9).abs() < 1e-12,
                            "seed {seed} {name}"
                        );
                    }
                    None => assert_eq!(expected[n].1, 0, "seed {seed} {name}"),
                }
            }
        }
    }

    #[test]
    fn run_file_roundtrip_and_cleanup() {
        let file = RunFile::create().unwrap();
        let path = file.path().to_path_buf();
        std::fs::write(&path, "hello 1\nstart x 1\nstop x 2\n").unwrap();
        let report = file.read().unwrap().unwrap();
        assert_eq!(report.regions["x"].count, 1);
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn names() {
        assert!(is_valid_name("sort_2"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("a b"));
        assert_eq!(metric_region("region:sort"), Some("sort"));
        assert_eq!(metric_region("sort"), None);
        assert_eq!(time_key("a"), "region:a");
        assert_eq!(count_key("a"), "region_count:a");
    }
}
