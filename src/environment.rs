//! Environment block and fingerprint of an export (`perfratio.environment`,
//! `perfratio.environment_fingerprint`, `--meta`, `--require-compatible-env`).
//! See `docs/specs/environment-fingerprint.md`.
//!
//! Only facts about the hardware class and the software the benchmarks ran on
//! are collected. Never the host name, the user name, environment variables,
//! the working directory or the full command line. A field the platform cannot
//! report is omitted, never guessed.

use std::collections::BTreeMap;
#[cfg(any(target_os = "linux", test))]
use std::path::Path;
use std::process::Command as Process;

use serde::{Deserialize, Serialize};

use crate::validation::sha256_hex;

/// Version of the fingerprint rule, hashed together with the fields
pub const FINGERPRINT_RULE: &str = "perfratio-environment-fingerprint/1";

/// The fields the fingerprint covers, in hash order
pub const FINGERPRINT_FIELDS: [&str; 7] = [
    "cpu_model",
    "arch",
    "os_family",
    "kernel_major",
    "governor",
    "smt",
    "container",
];

/// Container the benchmarks run in (Linux)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Container {
    /// `docker`, `podman`, `kubernetes` or `lxc`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// The perfratio build that ran the benchmarks
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BuildInfo {
    pub version: String,
    /// `release` or `debug`: a debug build is much slower
    pub profile: String,
    /// Cargo features this build was compiled with
    pub features: Vec<String>,
}

/// Run settings that change what is measured
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affinity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<String>,
}

impl Settings {
    fn is_empty(&self) -> bool {
        self == &Settings::default()
    }
}

/// The measurement backends in use and where they read from
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Backends {
    /// Energy backend (`sysfs_powercap`, `perf_pmu`, ...)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy: Option<String>,
    /// Hardware counter source (`perf_event_open` on Linux)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counters: Option<String>,
}

impl Backends {
    fn is_empty(&self) -> bool {
        self == &Backends::default()
    }
}

/// `perfratio.environment`
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Environment {
    /// `linux`, `macos`, `windows`, ... (`std::env::consts::OS`)
    pub os_family: String,
    pub arch: String,
    /// Kernel / OS release, e.g. `6.8.0-45-generic`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    /// First component of the release, e.g. `6`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_major: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_cores: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_cores: Option<usize>,
    /// Simultaneous multithreading (hyper-threading) is on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smt: Option<bool>,
    /// cpufreq governor of the CPUs (`mixed` when they differ)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governor: Option<String>,
    /// Turbo / boost is enabled
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turbo: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numa_nodes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    /// Present when a container was detected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,
    pub perfratio: BuildInfo,
    #[serde(skip_serializing_if = "Settings::is_empty")]
    pub settings: Settings,
    #[serde(skip_serializing_if = "Backends::is_empty")]
    pub backends: Backends,
}

/// One fingerprint field that differs between two environments
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDifference {
    pub field: &'static str,
    pub left: String,
    pub right: String,
}

impl Environment {
    /// The environment of this machine
    pub fn collect() -> Self {
        let mut env = Environment {
            os_family: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            kernel: crate::export::metadata::kernel_release(),
            cpu_model: crate::export::metadata::cpu_model(),
            logical_cores: std::thread::available_parallelism().ok().map(|n| n.get()),
            perfratio: BuildInfo::current(),
            ..Default::default()
        };
        #[cfg(target_os = "linux")]
        env.fill_from_linux_root(Path::new("/"));
        #[cfg(target_os = "macos")]
        env.fill_from_macos();
        env.kernel_major = env.kernel.as_deref().and_then(kernel_major);
        env
    }

    /// Fill what `/proc` and `/sys` below `root` tell. `root` is injectable for
    /// tests (see `tests/fixtures`-style trees in the unit tests).
    #[cfg(any(target_os = "linux", test))]
    pub fn fill_from_linux_root(&mut self, root: &Path) {
        use crate::system_check::{entries, read};
        let cpu_dirs: Vec<_> = entries(root, "/sys/devices/system/cpu", "cpu")
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n[3..].parse::<usize>().is_ok())
            })
            .collect();

        // Topology: unique (package, core) pairs are physical cores
        let mut cores = std::collections::BTreeSet::new();
        let mut logical = 0;
        for dir in &cpu_dirs {
            let topology = dir.join("topology");
            let rd = |name: &str| {
                std::fs::read_to_string(topology.join(name))
                    .ok()
                    .map(|s| s.trim().to_string())
            };
            if let (Some(package), Some(core)) = (rd("physical_package_id"), rd("core_id")) {
                cores.insert((package, core));
                logical += 1;
            }
        }
        if logical > 0 {
            self.logical_cores = Some(logical);
            self.physical_cores = Some(cores.len());
        }
        self.smt = match read(root, "/sys/devices/system/cpu/smt/active").as_deref() {
            Some("1") => Some(true),
            Some("0") => Some(false),
            _ => match (self.physical_cores, self.logical_cores) {
                (Some(physical), Some(logical)) if logical > 0 => Some(logical > physical),
                _ => None,
            },
        };

        let governors: std::collections::BTreeSet<String> = cpu_dirs
            .iter()
            .filter_map(|d| std::fs::read_to_string(d.join("cpufreq/scaling_governor")).ok())
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
            .collect();
        self.governor = match governors.len() {
            0 => None,
            1 => governors.into_iter().next(),
            _ => Some("mixed".to_string()),
        };

        self.turbo = match (
            read(root, "/sys/devices/system/cpu/intel_pstate/no_turbo").as_deref(),
            read(root, "/sys/devices/system/cpu/cpufreq/boost").as_deref(),
        ) {
            (Some("0"), _) => Some(true),
            (Some("1"), _) => Some(false),
            (None, Some("1")) => Some(true),
            (None, Some("0")) => Some(false),
            _ => None,
        };

        let nodes = entries(root, "/sys/devices/system/node", "node")
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n[4..].parse::<usize>().is_ok())
            })
            .count();
        self.numa_nodes = (nodes > 0).then_some(nodes);

        self.memory_bytes = read(root, "/proc/meminfo").as_deref().and_then(mem_total);

        if let Some(kind) = detect_container(
            root.join(".dockerenv").exists(),
            root.join("run/.containerenv").exists(),
            read(root, "/proc/1/cgroup").as_deref().unwrap_or(""),
        ) {
            self.container = Some(Container {
                kind: Some(kind.to_string()),
            });
        }
    }

    #[cfg(target_os = "macos")]
    fn fill_from_macos(&mut self) {
        let int = |name: &std::ffi::CStr| sysctl_u64(name);
        if let Some(n) = int(c"hw.logicalcpu") {
            self.logical_cores = Some(n as usize);
        }
        self.physical_cores = int(c"hw.physicalcpu").map(|n| n as usize);
        self.memory_bytes = int(c"hw.memsize");
        self.smt = match (self.physical_cores, self.logical_cores) {
            (Some(physical), Some(logical)) => Some(logical > physical),
            _ => None,
        };
    }

    /// Value of one fingerprint field; `None` when unknown. `container` is
    /// always known: `none` when no container was detected.
    fn fingerprint_value(&self, field: &str) -> Option<String> {
        match field {
            "cpu_model" => self.cpu_model.clone(),
            "arch" => Some(self.arch.clone()).filter(|a| !a.is_empty()),
            "os_family" => Some(self.os_family.clone()).filter(|a| !a.is_empty()),
            "kernel_major" => self.kernel_major.clone(),
            "governor" => self.governor.clone(),
            "smt" => self.smt.map(|b| b.to_string()),
            "container" => Some(
                self.container
                    .as_ref()
                    .and_then(|c| c.kind.clone())
                    .unwrap_or_else(|| "none".to_string()),
            ),
            _ => None,
        }
    }

    /// `sha256:<hex>` over [`FINGERPRINT_RULE`] and the [`FINGERPRINT_FIELDS`]
    /// as `name=value` lines in that order (`-` for an unknown value). Equal
    /// fingerprints mean the same hardware class.
    pub fn fingerprint(&self) -> String {
        let mut text = format!("{FINGERPRINT_RULE}\n");
        for field in FINGERPRINT_FIELDS {
            let value = self.fingerprint_value(field);
            text.push_str(&format!("{field}={}\n", value.as_deref().unwrap_or("-")));
        }
        format!("sha256:{}", sha256_hex(text.as_bytes()))
    }

    /// The fingerprint fields whose values are known on both sides and differ.
    /// A field unknown on one side is not a difference.
    pub fn differences(&self, other: &Environment) -> Vec<FieldDifference> {
        FINGERPRINT_FIELDS
            .iter()
            .filter_map(|&field| {
                let (left, right) = (
                    self.fingerprint_value(field)?,
                    other.fingerprint_value(field)?,
                );
                (left != right).then_some(FieldDifference { field, left, right })
            })
            .collect()
    }
}

impl BuildInfo {
    pub fn current() -> Self {
        BuildInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_string(),
            features: if cfg!(feature = "cli") {
                vec!["cli".to_string()]
            } else {
                vec![]
            },
        }
    }
}

/// `6.8.0-45-generic` -> `6`
pub fn kernel_major(release: &str) -> Option<String> {
    let digits: String = release.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then_some(digits)
}

/// `MemTotal:       16384 kB` in `/proc/meminfo` -> bytes
#[cfg(any(target_os = "linux", test))]
fn mem_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let mut parts = line.split_whitespace().skip(1);
    let value: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") => value.checked_mul(1024),
        Some(_) => None,
        None => Some(value),
    }
}

/// Which container runtime the markers point to, if any. `cgroup` is the text
/// of `/proc/1/cgroup`. Kubernetes first: its pods are also docker/containerd
/// containers.
#[cfg(any(target_os = "linux", test))]
fn detect_container(dockerenv: bool, containerenv: bool, cgroup: &str) -> Option<&'static str> {
    if cgroup.contains("kubepods") {
        Some("kubernetes")
    } else if containerenv {
        Some("podman")
    } else if dockerenv || cgroup.contains("/docker/") || cgroup.contains("docker-") {
        Some("docker")
    } else if cgroup.contains("/lxc/") || cgroup.contains("/lxc.payload") {
        Some("lxc")
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut len = buf.len();
    // SAFETY: `name` is NUL-terminated; `buf` has room for `len` bytes, and
    // sysctlbyname writes at most that many and updates `len`.
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret != 0 {
        return None;
    }
    match len {
        4 => Some(u32::from_ne_bytes(buf[..4].try_into().ok()?) as u64),
        8 => Some(u64::from_ne_bytes(buf)),
        _ => None,
    }
}

/// `--meta`: `KEY=VALUE` pairs, and the bare word `git`
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MetaRequest {
    pub pairs: BTreeMap<String, String>,
    pub git: bool,
}

/// Parse the `--meta` arguments. `git` asks for the commit and dirty flag of
/// the working directory; every other argument is `KEY=VALUE`.
pub fn parse_meta<'a>(args: impl IntoIterator<Item = &'a str>) -> anyhow::Result<MetaRequest> {
    let mut request = MetaRequest::default();
    let mut pairs = vec![];
    for arg in args {
        if arg == "git" {
            request.git = true;
        } else {
            pairs.push(arg);
        }
    }
    request.pairs = crate::export::metadata::parse_labels(pairs)
        .map_err(|e| anyhow::anyhow!("{}", e.to_string().replace("label", "--meta")))?;
    for key in ["git_commit", "git_dirty"] {
        if request.git && request.pairs.contains_key(key) {
            anyhow::bail!("Invalid --meta: '{key}' is set by '--meta git'");
        }
    }
    Ok(request)
}

/// Commit and dirty flag from the output of `git rev-parse HEAD` and
/// `git status --porcelain`
pub fn parse_git(rev_parse: &str, status: &str) -> Option<(String, bool)> {
    let commit = rev_parse.trim();
    let valid = (40..=64).contains(&commit.len()) && commit.chars().all(|c| c.is_ascii_hexdigit());
    valid.then(|| (commit.to_ascii_lowercase(), !status.trim().is_empty()))
}

/// Commit and dirty flag of the git repository in the working directory.
/// `None` outside a repository or without git.
pub fn git_provenance() -> Option<(String, bool)> {
    let run = |args: &[&str]| -> Option<String> {
        let out = Process::new("git").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    parse_git(
        &run(&["rev-parse", "HEAD"])?,
        &run(&["status", "--porcelain"])?,
    )
}

/// The `perfratio.meta` object for a request. Warns on stderr when `git` was
/// asked for and the working directory is not a repository.
pub fn resolve_meta(request: &MetaRequest) -> BTreeMap<String, String> {
    let mut meta = request.pairs.clone();
    if request.git {
        match git_provenance() {
            Some((commit, dirty)) => {
                meta.insert("git_commit".to_string(), commit);
                meta.insert("git_dirty".to_string(), dirty.to_string());
            }
            None => eprintln!(
                "{} '--meta git': the working directory is not a git repository (or git is \
                 missing); no git provenance is recorded.",
                crate::output::colors::yellow("Warning:")
            ),
        }
    }
    meta
}

/// The terminal warning for an environment mismatch, or `None` when the
/// environments are compatible
pub fn mismatch_message(
    what: &str,
    baseline: &Environment,
    current: &Environment,
) -> Option<String> {
    let differences = baseline.differences(current);
    if differences.is_empty() {
        return None;
    }
    let list: Vec<String> = differences
        .iter()
        .map(|d| {
            format!(
                "{} ({} -> {})",
                d.field,
                crate::util::sanitize::escape_control_chars(&d.left),
                crate::util::sanitize::escape_control_chars(&d.right)
            )
        })
        .collect();
    Some(format!(
        "{what} was measured in a different environment: {}. Results from different hardware \
         classes or settings may not be comparable.",
        list.join(", ")
    ))
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

    /// Two physical cores with two threads each, one NUMA node, performance governor
    fn fixture(governor: &str, smt: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for cpu in 0..4 {
            let base = format!("sys/devices/system/cpu/cpu{cpu}");
            write(root, &format!("{base}/topology/physical_package_id"), "0\n");
            write(
                root,
                &format!("{base}/topology/core_id"),
                &format!("{}\n", cpu % 2),
            );
            write(
                root,
                &format!("{base}/cpufreq/scaling_governor"),
                &format!("{governor}\n"),
            );
        }
        write(root, "sys/devices/system/cpu/smt/active", smt);
        write(root, "sys/devices/system/cpu/intel_pstate/no_turbo", "0\n");
        write(root, "sys/devices/system/node/node0/cpulist", "0-3\n");
        write(
            root,
            "proc/meminfo",
            "MemTotal:       16384 kB\nMemFree: 1 kB\n",
        );
        write(root, "proc/1/cgroup", "0::/\n");
        dir
    }

    fn linux_env(root: &Path) -> Environment {
        let mut env = Environment {
            os_family: "linux".into(),
            arch: "x86_64".into(),
            kernel: Some("6.8.0-45-generic".into()),
            kernel_major: Some("6".into()),
            cpu_model: Some("Test CPU @ 3GHz".into()),
            ..Default::default()
        };
        env.fill_from_linux_root(root);
        env
    }

    #[test]
    fn reads_the_linux_topology() {
        let dir = fixture("performance", "1\n");
        let env = linux_env(dir.path());
        assert_eq!(env.logical_cores, Some(4));
        assert_eq!(env.physical_cores, Some(2));
        assert_eq!(env.smt, Some(true));
        assert_eq!(env.governor.as_deref(), Some("performance"));
        assert_eq!(env.turbo, Some(true));
        assert_eq!(env.numa_nodes, Some(1));
        assert_eq!(env.memory_bytes, Some(16384 * 1024));
        assert_eq!(env.container, None);
    }

    #[test]
    fn smt_falls_back_to_the_topology() {
        let dir = fixture("performance", "x");
        fs::remove_file(dir.path().join("sys/devices/system/cpu/smt/active")).unwrap();
        assert_eq!(linux_env(dir.path()).smt, Some(true));
    }

    #[test]
    fn differing_governors_are_mixed() {
        let dir = fixture("performance", "1\n");
        write(
            dir.path(),
            "sys/devices/system/cpu/cpu3/cpufreq/scaling_governor",
            "powersave\n",
        );
        assert_eq!(linux_env(dir.path()).governor.as_deref(), Some("mixed"));
    }

    #[test]
    fn an_empty_root_fills_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let env = linux_env(dir.path());
        assert_eq!(env.logical_cores, None);
        assert_eq!(env.smt, None);
        assert_eq!(env.governor, None);
        assert_eq!(env.turbo, None);
        assert_eq!(env.memory_bytes, None);
    }

    #[test]
    fn fingerprint_is_stable_and_follows_governor_and_smt() {
        let a = linux_env(fixture("performance", "1\n").path());
        let b = linux_env(fixture("performance", "1\n").path());
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(a.fingerprint().starts_with("sha256:"));
        assert_eq!(a.fingerprint().len(), 7 + 64);
        let powersave = linux_env(fixture("powersave", "1\n").path());
        assert_ne!(a.fingerprint(), powersave.fingerprint());
        let no_smt = linux_env(fixture("performance", "0\n").path());
        assert_ne!(a.fingerprint(), no_smt.fingerprint());
    }

    #[test]
    fn fingerprint_ignores_fields_outside_the_documented_subset() {
        let mut a = linux_env(fixture("performance", "1\n").path());
        let mut b = a.clone();
        b.kernel = Some("6.9.1".into());
        b.memory_bytes = Some(1);
        b.numa_nodes = Some(8);
        b.turbo = Some(false);
        b.settings.affinity = Some("0-1".into());
        b.perfratio.version = "9.9.9".into();
        assert_eq!(a.fingerprint(), b.fingerprint());
        a.kernel_major = Some("7".into());
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_fixed_value() {
        // The rule is a contract: a change of this value is a contract change
        let env = Environment {
            os_family: "linux".into(),
            arch: "x86_64".into(),
            kernel_major: Some("6".into()),
            cpu_model: Some("Test CPU".into()),
            governor: Some("performance".into()),
            smt: Some(true),
            ..Default::default()
        };
        let text = "perfratio-environment-fingerprint/1\ncpu_model=Test CPU\narch=x86_64\n\
                    os_family=linux\nkernel_major=6\ngovernor=performance\nsmt=true\ncontainer=none\n";
        assert_eq!(
            env.fingerprint(),
            format!("sha256:{}", sha256_hex(text.as_bytes()))
        );
    }

    #[test]
    fn differences_list_the_fields_known_on_both_sides() {
        let a = linux_env(fixture("performance", "1\n").path());
        let mut b = linux_env(fixture("powersave", "1\n").path());
        b.cpu_model = Some("Other CPU".into());
        let fields: Vec<_> = a.differences(&b).iter().map(|d| d.field).collect();
        assert_eq!(fields, ["cpu_model", "governor"]);
        // unknown on one side is not a difference
        b.governor = None;
        let fields: Vec<_> = a.differences(&b).iter().map(|d| d.field).collect();
        assert_eq!(fields, ["cpu_model"]);
        assert!(a.differences(&a).is_empty());
        assert!(mismatch_message("x", &a, &a).is_none());
        let message = mismatch_message("The baseline", &a, &b).unwrap();
        assert!(
            message.contains("cpu_model (Test CPU @ 3GHz -> Other CPU)"),
            "{message}"
        );
    }

    #[test]
    fn detects_containers() {
        assert_eq!(detect_container(false, false, "0::/"), None);
        assert_eq!(detect_container(true, false, "0::/"), Some("docker"));
        assert_eq!(detect_container(false, true, "0::/"), Some("podman"));
        assert_eq!(
            detect_container(true, false, "0::/kubepods/besteffort/pod1"),
            Some("kubernetes")
        );
        assert_eq!(
            detect_container(false, false, "12:cpu:/docker/abc"),
            Some("docker")
        );
        assert_eq!(
            detect_container(false, false, "1:name=systemd:/lxc/c1"),
            Some("lxc")
        );
    }

    #[test]
    fn container_comes_from_the_root() {
        let dir = fixture("performance", "1\n");
        write(dir.path(), ".dockerenv", "");
        let env = linux_env(dir.path());
        assert_eq!(env.container.unwrap().kind.as_deref(), Some("docker"));
    }

    #[test]
    fn parses_proc_values() {
        assert_eq!(kernel_major("6.8.0-45-generic").as_deref(), Some("6"));
        assert_eq!(kernel_major("24.1.0").as_deref(), Some("24"));
        assert_eq!(kernel_major("generic"), None);
        assert_eq!(mem_total("MemTotal: 2 kB\n"), Some(2048));
        assert_eq!(mem_total("MemFree: 2 kB\n"), None);
        assert_eq!(mem_total("MemTotal: 2 MB\n"), None);
    }

    #[test]
    fn meta_pairs_and_git() {
        let request = parse_meta(["build=release", "git", "compiler=gcc-14"]).unwrap();
        assert!(request.git);
        assert_eq!(request.pairs["build"], "release");
        assert_eq!(request.pairs["compiler"], "gcc-14");
        assert!(parse_meta(["noequals"]).is_err());
        assert!(parse_meta(["a=1", "a=2"]).is_err());
        assert!(parse_meta(["git", "git_commit=abc"]).is_err());
        let error = parse_meta(["bad key=1"]).unwrap_err().to_string();
        assert!(error.contains("--meta"), "{error}");
    }

    #[test]
    fn parses_git_output() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            parse_git(&format!("{sha}\n"), ""),
            Some((sha.to_string(), false))
        );
        assert_eq!(
            parse_git(sha, " M src/main.rs\n"),
            Some((sha.to_string(), true))
        );
        assert_eq!(parse_git("fatal: not a git repository", ""), None);
        assert_eq!(parse_git("", ""), None);
    }

    #[test]
    fn serializes_without_unknown_fields_and_round_trips() {
        let env = Environment {
            os_family: "linux".into(),
            arch: "x86_64".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&env).unwrap();
        let keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["arch", "os_family", "perfratio"]);
        let back: Environment = serde_json::from_value(json).unwrap();
        assert_eq!(back, env);
    }
}
