//! cgroup CPU quota, memory limit and throttling counters (Linux, v1 and v2).
//! See `docs/specs/cgroup-thermal.md`.
//!
//! A container or a `systemd-run -p CPUQuota=` scope that is throttled runs
//! slower for reasons that have nothing to do with the benchmarked program.
//! The functions read below an injectable root (`/proc/self/cgroup` and
//! `/sys/fs/cgroup`), so tests use fixture trees. A counter that cannot be read
//! is `None`, never "not throttled".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Memory limits at or above this are the kernel's "unlimited" (v1 reports a page-rounded `i64::MAX`)
const UNLIMITED_BYTES: u64 = 1 << 60;

/// The cgroup hierarchy the process lives in
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "u8", try_from = "u8")]
pub enum Version {
    V1,
    V2,
}

impl From<Version> for u8 {
    fn from(v: Version) -> u8 {
        match v {
            Version::V1 => 1,
            Version::V2 => 2,
        }
    }
}

impl TryFrom<u8> for Version {
    type Error = String;
    fn try_from(v: u8) -> Result<Self, String> {
        match v {
            1 => Ok(Version::V1),
            2 => Ok(Version::V2),
            other => Err(format!("unknown cgroup version {other}")),
        }
    }
}

/// The directories whose files apply to this process, own cgroup first and
/// then its ancestors (a limit or a throttle counter of a parent applies too)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cgroup {
    pub version: Version,
    /// CPU controller directories, own cgroup first
    pub cpu_dirs: Vec<PathBuf>,
    /// Memory controller directories, own cgroup first
    pub memory_dirs: Vec<PathBuf>,
}

/// Cumulative throttling counters, summed over the cgroup and its ancestors
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Time the cgroup was throttled, microseconds
    pub throttled_usec: u64,
    /// Number of periods in which it was throttled
    pub nr_throttled: u64,
    /// Processes killed by the out-of-memory killer, when readable
    pub oom_kills: Option<u64>,
}

/// Limits of the cgroup: `environment.container` takes them
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Limits {
    pub version: Option<Version>,
    /// CPU quota in CPUs (`cpu.max` 20000/100000 is 0.2); the smallest of the cgroup and its ancestors
    pub cpu_quota: Option<f64>,
    /// Memory limit in bytes; the smallest of the cgroup and its ancestors
    pub memory_limit: Option<u64>,
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// `key value` lines to the value of `key`
fn field(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once(char::is_whitespace)?;
        (k == key).then(|| v.trim().parse().ok()).flatten()
    })
}

/// Find the cgroup of this process below `root`. `None` without cgroup files.
pub fn locate(root: &Path) -> Option<Cgroup> {
    let proc_cgroup = read(&root.join("proc/self/cgroup"))?;
    let base = root.join("sys/fs/cgroup");
    let mut v2_path = None;
    let mut cpu_path = None;
    let mut memory_path = None;
    for line in proc_cgroup.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(_id), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if controllers.is_empty() {
            v2_path = Some(path);
            continue;
        }
        for controller in controllers.split(',') {
            match controller {
                "cpu" => cpu_path = Some(path),
                "memory" => memory_path = Some(path),
                _ => {}
            }
        }
    }
    // The unified hierarchy wins when it has the files (hybrid layouts list both)
    if let Some(path) = v2_path {
        let own = base.join(path.trim_start_matches('/'));
        if base.join("cgroup.controllers").exists() || own.join("cpu.stat").exists() {
            let dirs = chain(&base, &own);
            return Some(Cgroup {
                version: Version::V2,
                cpu_dirs: dirs.clone(),
                memory_dirs: dirs,
            });
        }
    }
    let dir = |controller: &str, path: Option<&str>| {
        let path = path?.trim_start_matches('/');
        // `cpu,cpuacct` is one mount in most v1 layouts, `cpu` a symlink to it
        let candidates: &[&str] = if controller == "cpu" {
            &["cpu", "cpu,cpuacct"]
        } else {
            &["memory"]
        };
        candidates
            .iter()
            .map(|c| base.join(c))
            .find(|d| d.is_dir())
            .map(|mount| chain(&mount, &mount.join(path)))
    };
    let cpu_dirs = dir("cpu", cpu_path)?;
    let memory_dirs = dir("memory", memory_path).unwrap_or_default();
    Some(Cgroup {
        version: Version::V1,
        cpu_dirs,
        memory_dirs,
    })
}

/// `own`, then its parents up to and including `base`
fn chain(base: &Path, own: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![];
    let mut dir = own.to_path_buf();
    loop {
        dirs.push(dir.clone());
        if dir == base || !dir.starts_with(base) || !dir.pop() {
            break;
        }
    }
    dirs
}

/// `max 100000` -> unlimited, `20000 100000` -> 0.2 CPUs
pub fn parse_cpu_max(text: &str) -> Option<f64> {
    let mut parts = text.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let period: f64 = parts.next().unwrap_or("100000").parse().ok()?;
    let quota: f64 = quota.parse().ok()?;
    (period > 0.0 && quota > 0.0).then(|| quota / period)
}

/// cgroup v1: `cpu.cfs_quota_us` (-1 is unlimited) over `cpu.cfs_period_us`
pub fn parse_cfs(quota: &str, period: &str) -> Option<f64> {
    let quota: f64 = quota.trim().parse().ok()?;
    let period: f64 = period.trim().parse().ok()?;
    (quota > 0.0 && period > 0.0).then(|| quota / period)
}

fn parse_memory(text: &str) -> Option<u64> {
    let value: u64 = text.trim().parse().ok()?;
    (value < UNLIMITED_BYTES).then_some(value)
}

impl Cgroup {
    pub fn limits(&self) -> Limits {
        let cpu = self
            .cpu_dirs
            .iter()
            .filter_map(|dir| match self.version {
                Version::V2 => parse_cpu_max(&read(&dir.join("cpu.max"))?),
                Version::V1 => parse_cfs(
                    &read(&dir.join("cpu.cfs_quota_us"))?,
                    &read(&dir.join("cpu.cfs_period_us"))?,
                ),
            })
            .reduce(f64::min);
        let memory = self
            .memory_dirs
            .iter()
            .filter_map(|dir| {
                let file = match self.version {
                    Version::V2 => "memory.max",
                    Version::V1 => "memory.limit_in_bytes",
                };
                parse_memory(&read(&dir.join(file))?)
            })
            .reduce(u64::min);
        Limits {
            version: Some(self.version),
            cpu_quota: cpu,
            memory_limit: memory,
        }
    }

    /// Current throttling counters. `None` when no cgroup of the chain exposes
    /// them (v2: the `cpu` controller is not enabled there), so "cannot tell"
    /// is never read as "not throttled".
    pub fn snapshot(&self) -> Option<Snapshot> {
        let mut total: Option<Snapshot> = None;
        for dir in &self.cpu_dirs {
            let Some(text) = read(&dir.join("cpu.stat")) else {
                continue;
            };
            let (nr, usec) = match self.version {
                Version::V2 => (field(&text, "nr_throttled"), field(&text, "throttled_usec")),
                // v1 counts nanoseconds
                Version::V1 => (
                    field(&text, "nr_throttled"),
                    field(&text, "throttled_time").map(|ns| ns / 1000),
                ),
            };
            let (Some(nr), Some(usec)) = (nr, usec) else {
                continue;
            };
            let total = total.get_or_insert_with(Snapshot::default);
            total.nr_throttled += nr;
            total.throttled_usec += usec;
        }
        let mut snapshot = total?;
        snapshot.oom_kills = self.oom_kills();
        Some(snapshot)
    }

    /// CPU time consumed by the own cgroup and its descendants so far, in seconds.
    /// v2: `usage_usec` of `cpu.stat`; v1: `cpuacct.usage` (ns), when `cpuacct` shares the
    /// `cpu` directory. `None` when the file is missing (controller off), never 0.
    pub fn cpu_usage_seconds(&self) -> Option<f64> {
        let dir = self.cpu_dirs.first()?;
        match self.version {
            Version::V2 => {
                field(&read(&dir.join("cpu.stat"))?, "usage_usec").map(|u| u as f64 / 1e6)
            }
            Version::V1 => read(&dir.join("cpuacct.usage"))?
                .trim()
                .parse::<u64>()
                .ok()
                .map(|ns| ns as f64 / 1e9),
        }
    }

    fn oom_kills(&self) -> Option<u64> {
        let mut total = None;
        for dir in &self.memory_dirs {
            let events = match self.version {
                Version::V2 => read(&dir.join("memory.events")),
                Version::V1 => read(&dir.join("memory.oom_control")),
            };
            if let Some(kills) = events.and_then(|t| field(&t, "oom_kill")) {
                *total.get_or_insert(0) += kills;
            }
        }
        total
    }
}

impl Snapshot {
    /// What grew since `earlier` (counters never go down; a reset reads as 0)
    pub fn since(&self, earlier: &Snapshot) -> Snapshot {
        Snapshot {
            throttled_usec: self.throttled_usec.saturating_sub(earlier.throttled_usec),
            nr_throttled: self.nr_throttled.saturating_sub(earlier.nr_throttled),
            oom_kills: match (self.oom_kills, earlier.oom_kills) {
                (Some(now), Some(before)) => Some(now.saturating_sub(before)),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, path: &str, content: &str) {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    /// v2: `/sys/fs/cgroup/system.slice/run-1.scope`, quota 20% on the scope
    fn v2_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "proc/self/cgroup", "0::/system.slice/run-1.scope\n");
        write(r, "sys/fs/cgroup/cgroup.controllers", "cpu memory\n");
        let scope = "sys/fs/cgroup/system.slice/run-1.scope";
        write(r, &format!("{scope}/cpu.max"), "20000 100000\n");
        write(
            r,
            &format!("{scope}/cpu.stat"),
            "usage_usec 500\nnr_periods 40\nnr_throttled 12\nthrottled_usec 345000\n",
        );
        write(r, &format!("{scope}/memory.max"), "536870912\n");
        write(
            r,
            &format!("{scope}/memory.events"),
            "low 0\nmax 4\noom 1\noom_kill 1\n",
        );
        write(r, "sys/fs/cgroup/system.slice/cpu.max", "max 100000\n");
        write(r, "sys/fs/cgroup/system.slice/cpu.stat", "usage_usec 1\n");
        write(r, "sys/fs/cgroup/system.slice/memory.max", "max\n");
        dir
    }

    #[test]
    fn v2_limits_and_counters() {
        let dir = v2_fixture();
        let cgroup = locate(dir.path()).unwrap();
        assert_eq!(cgroup.version, Version::V2);
        let limits = cgroup.limits();
        assert_eq!(limits.version, Some(Version::V2));
        assert_eq!(limits.cpu_quota, Some(0.2));
        assert_eq!(limits.memory_limit, Some(536_870_912));
        let snapshot = cgroup.snapshot().unwrap();
        assert_eq!(snapshot.nr_throttled, 12);
        assert_eq!(snapshot.throttled_usec, 345_000);
        assert_eq!(snapshot.oom_kills, Some(1));
    }

    #[test]
    fn a_parent_quota_applies_to_a_child() {
        let dir = v2_fixture();
        let r = dir.path();
        fs::remove_file(r.join("sys/fs/cgroup/system.slice/run-1.scope/cpu.max")).unwrap();
        write(r, "sys/fs/cgroup/system.slice/cpu.max", "150000 100000\n");
        write(
            r,
            "sys/fs/cgroup/system.slice/cpu.stat",
            "nr_throttled 3\nthrottled_usec 1000\n",
        );
        let cgroup = locate(r).unwrap();
        assert_eq!(cgroup.limits().cpu_quota, Some(1.5));
        // own 12 + parent 3
        assert_eq!(cgroup.snapshot().unwrap().nr_throttled, 15);
    }

    #[test]
    fn unlimited_is_none_and_unreadable_counters_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "proc/self/cgroup", "0::/\n");
        write(r, "sys/fs/cgroup/cgroup.controllers", "cpu\n");
        write(r, "sys/fs/cgroup/cpu.max", "max 100000\n");
        write(r, "sys/fs/cgroup/memory.max", "max\n");
        // the cpu controller is not enabled here: no throttle keys
        write(r, "sys/fs/cgroup/cpu.stat", "usage_usec 10\nuser_usec 5\n");
        let cgroup = locate(r).unwrap();
        let limits = cgroup.limits();
        assert_eq!((limits.cpu_quota, limits.memory_limit), (None, None));
        assert_eq!(cgroup.snapshot(), None);
    }

    #[test]
    fn cpu_usage_is_read_from_cpu_stat_v2_and_cpuacct_v1() {
        let dir = v2_fixture();
        let cgroup = locate(dir.path()).unwrap();
        assert_eq!(cgroup.cpu_usage_seconds(), Some(0.0005));

        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "proc/self/cgroup", "4:cpu,cpuacct:/docker/abc\n");
        let cpu = "sys/fs/cgroup/cpu,cpuacct/docker/abc";
        write(r, &format!("{cpu}/cpu.stat"), "nr_periods 1\n");
        assert_eq!(locate(r).unwrap().cpu_usage_seconds(), None);
        write(r, &format!("{cpu}/cpuacct.usage"), "2500000000\n");
        assert_eq!(locate(r).unwrap().cpu_usage_seconds(), Some(2.5));
    }

    #[test]
    fn no_cgroup_files_locate_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(locate(dir.path()), None);
    }

    /// v1: separate `cpu,cpuacct` and `memory` mounts
    #[test]
    fn v1_layout() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(
            r,
            "proc/self/cgroup",
            "12:memory:/docker/abc\n4:cpu,cpuacct:/docker/abc\n1:name=systemd:/docker/abc\n",
        );
        let cpu = "sys/fs/cgroup/cpu,cpuacct/docker/abc";
        write(r, &format!("{cpu}/cpu.cfs_quota_us"), "50000\n");
        write(r, &format!("{cpu}/cpu.cfs_period_us"), "100000\n");
        write(
            r,
            &format!("{cpu}/cpu.stat"),
            "nr_periods 10\nnr_throttled 4\nthrottled_time 2500000\n",
        );
        let mem = "sys/fs/cgroup/memory/docker/abc";
        write(r, &format!("{mem}/memory.limit_in_bytes"), "1073741824\n");
        write(
            r,
            &format!("{mem}/memory.oom_control"),
            "oom_kill_disable 0\nunder_oom 0\noom_kill 2\n",
        );
        let cgroup = locate(r).unwrap();
        assert_eq!(cgroup.version, Version::V1);
        let limits = cgroup.limits();
        assert_eq!(limits.version, Some(Version::V1));
        assert_eq!(limits.cpu_quota, Some(0.5));
        assert_eq!(limits.memory_limit, Some(1 << 30));
        let snapshot = cgroup.snapshot().unwrap();
        assert_eq!(snapshot.nr_throttled, 4);
        assert_eq!(snapshot.throttled_usec, 2500);
        assert_eq!(snapshot.oom_kills, Some(2));
    }

    #[test]
    fn v1_unlimited_values() {
        assert_eq!(parse_cfs("-1", "100000"), None);
        assert_eq!(parse_cfs("100000", "100000"), Some(1.0));
        assert_eq!(parse_memory("9223372036854771712"), None);
        assert_eq!(parse_memory("1048576"), Some(1_048_576));
    }

    #[test]
    fn parses_cpu_max() {
        assert_eq!(parse_cpu_max("max 100000"), None);
        assert_eq!(parse_cpu_max("max"), None);
        assert_eq!(parse_cpu_max("200000 100000"), Some(2.0));
        assert_eq!(parse_cpu_max("50000"), Some(0.5));
        assert_eq!(parse_cpu_max("garbage"), None);
    }

    #[test]
    fn snapshots_subtract_without_underflow() {
        let a = Snapshot {
            throttled_usec: 100,
            nr_throttled: 5,
            oom_kills: Some(1),
        };
        let b = Snapshot {
            throttled_usec: 250,
            nr_throttled: 7,
            oom_kills: Some(1),
        };
        assert_eq!(
            b.since(&a),
            Snapshot {
                throttled_usec: 150,
                nr_throttled: 2,
                oom_kills: Some(0)
            }
        );
        assert_eq!(a.since(&b).throttled_usec, 0);
    }

    #[test]
    fn version_serializes_as_a_number() {
        assert_eq!(serde_json::to_string(&Version::V2).unwrap(), "2");
        assert_eq!(serde_json::from_str::<Version>("1").unwrap(), Version::V1);
        assert!(serde_json::from_str::<Version>("3").is_err());
    }
}
