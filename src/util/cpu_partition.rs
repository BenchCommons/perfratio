//! `--pin-workers` / `--reserve-cpus`: divide the allowed CPUs among the
//! workers of `--jobs` and keep some CPUs for perfratio itself (#307).
//!
//! [`partition`] is a pure function of the topology, so the policy is tested
//! without touching the machine. [`discover`] reads the real topology.

use anyhow::{bail, ensure, Result};
use serde::Serialize;

/// CPUs the benchmark may use and how they group into physical cores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    /// CPUs perfratio may use (`--affinity`, or the CPUs of the process)
    pub allowed: Vec<usize>,
    /// Hardware threads of one physical core (SMT siblings). CPUs that are in
    /// no group count as a core of their own.
    pub cores: Vec<Vec<usize>>,
}

/// `--reserve-cpus`
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reserve {
    /// The N highest allowed CPUs, plus their SMT siblings
    Count(usize),
    /// Exactly these CPUs
    List(Vec<usize>),
}

impl Reserve {
    /// `4` (a count) or `0,15` / `0-1` (a CPU list). A bare number is a count.
    pub fn parse(value: &str) -> Result<Self> {
        let value = value.trim();
        if !value.contains([',', '-']) {
            if let Ok(count) = value.parse::<usize>() {
                ensure!(
                    count >= 1,
                    "'{value}': the number of CPUs must be >= 1 (use '0-0' to reserve CPU 0)"
                );
                return Ok(Reserve::Count(count));
            }
        }
        super::affinity::parse_cpu_list(value).map(Reserve::List)
    }
}

/// The CPUs of every worker and the CPUs kept for perfratio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Partition {
    /// CPUs of worker `i` (disjoint, sorted)
    pub workers: Vec<Vec<usize>>,
    /// CPUs no worker uses, for perfratio's own threads
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reserved: Vec<usize>,
    /// Workers share a physical core (more workers than physical cores)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub shared_siblings: bool,
}

/// Divide the usable CPUs among `jobs` workers: one physical core per worker
/// (all its hardware threads, kept together), the lowest cores first. With
/// fewer cores than workers, every worker gets one hardware thread and the
/// siblings of a core are shared (`shared_siblings`).
pub fn partition(topology: &Topology, reserve: Option<&Reserve>, jobs: usize) -> Result<Partition> {
    let mut allowed = topology.allowed.clone();
    allowed.sort_unstable();
    allowed.dedup();
    let sibling_group = |cpu: usize| topology.cores.iter().find(|group| group.contains(&cpu));

    let mut reserved: Vec<usize> = match reserve {
        None => Vec::new(),
        Some(Reserve::List(list)) => {
            if let Some(cpu) = list.iter().find(|cpu| !allowed.contains(cpu)) {
                bail!("'--reserve-cpus': CPU {cpu} is not one of the usable CPUs {allowed:?}");
            }
            list.clone()
        }
        Some(Reserve::Count(count)) => {
            ensure!(
                *count < allowed.len(),
                "'--reserve-cpus {count}': the system has only {} usable CPUs, none would be left \
                 for the workers",
                allowed.len()
            );
            let mut picked = Vec::new();
            for &cpu in allowed.iter().rev().take(*count) {
                picked.push(cpu);
                // A worker on the sibling of a reserved CPU would share its core
                if let Some(group) = sibling_group(cpu) {
                    picked.extend(group.iter().copied().filter(|c| allowed.contains(c)));
                }
            }
            picked
        }
    };
    reserved.sort_unstable();
    reserved.dedup();

    let usable: Vec<usize> = allowed
        .iter()
        .copied()
        .filter(|cpu| !reserved.contains(cpu))
        .collect();
    ensure!(
        usable.len() >= jobs,
        "'--jobs {jobs}' needs {jobs} CPUs, but only {} are usable{}. Use fewer jobs or \
         '--pin-workers=none'",
        usable.len(),
        if reserved.is_empty() {
            String::new()
        } else {
            format!(" after reserving {reserved:?}")
        }
    );

    // The usable hardware threads of every physical core, lowest core first
    let mut cores: Vec<Vec<usize>> = Vec::new();
    for &cpu in &usable {
        if cores.iter().any(|core| core.contains(&cpu)) {
            continue;
        }
        let mut core: Vec<usize> = match sibling_group(cpu) {
            Some(group) => group
                .iter()
                .copied()
                .filter(|c| usable.contains(c))
                .collect(),
            None => vec![cpu],
        };
        core.sort_unstable();
        cores.push(core);
    }

    let (workers, shared_siblings) = if cores.len() >= jobs {
        (cores.into_iter().take(jobs).collect::<Vec<_>>(), false)
    } else {
        // First the first thread of every core, then the second ones, ...
        let width = cores.iter().map(Vec::len).max().unwrap_or(0);
        let order: Vec<usize> = (0..width)
            .flat_map(|t| cores.iter().filter_map(move |core| core.get(t).copied()))
            .collect();
        let workers: Vec<Vec<usize>> = order.into_iter().take(jobs).map(|cpu| vec![cpu]).collect();
        let shared = cores
            .iter()
            .any(|core| workers.iter().filter(|w| core.contains(&w[0])).count() > 1);
        (workers, shared)
    };
    Ok(Partition {
        workers,
        reserved,
        shared_siblings,
    })
}

/// CPUs of the process and the SMT sibling groups among them.
pub fn discover(affinity: Option<&[usize]>) -> Topology {
    let mut allowed: Vec<usize> = match affinity {
        Some(cpus) => cpus.to_vec(),
        None => process_cpus(),
    };
    // SetProcessAffinityMask covers one processor group of at most usize::BITS CPUs
    if cfg!(windows) {
        allowed.retain(|&cpu| cpu < usize::BITS as usize);
    }
    allowed.sort_unstable();
    allowed.dedup();
    let cores = smt_groups(&allowed);
    Topology { allowed, cores }
}

/// CPUs this process may run on.
fn process_cpus() -> Vec<usize> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: an all-zero cpu_set_t is the empty set; sched_getaffinity
        // writes at most the given size into it.
        let set = unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
                Some(set)
            } else {
                None
            }
        };
        if let Some(set) = set {
            // SAFETY: CPU_ISSET only reads `set`; the index is below CPU_SETSIZE.
            let cpus: Vec<usize> = (0..libc::CPU_SETSIZE as usize)
                .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &set) })
                .collect();
            if !cpus.is_empty() {
                return cpus;
            }
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask};
        let (mut process, mut system) = (0usize, 0usize);
        // SAFETY: both pointers refer to live usize values.
        let ok = unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut process, &mut system) };
        if ok != 0 && process != 0 {
            return (0..usize::BITS as usize)
                .filter(|cpu| process >> cpu & 1 == 1)
                .collect();
        }
    }
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    (0..n).collect()
}

/// Hardware threads that share a physical core (Linux `thread_siblings_list`).
/// Elsewhere the siblings are unknown and every CPU counts as a core.
fn smt_groups(allowed: &[usize]) -> Vec<Vec<usize>> {
    #[cfg(target_os = "linux")]
    {
        let mut groups: Vec<Vec<usize>> = Vec::new();
        for &cpu in allowed {
            let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list");
            let Some(list) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| super::affinity::parse_cpu_list(text.trim()).ok())
            else {
                continue;
            };
            if !groups.contains(&list) {
                groups.push(list);
            }
        }
        groups
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = allowed;
        Vec::new()
    }
}

/// Whether workers can be pinned on this platform (Linux, Windows).
pub fn supported() -> bool {
    cfg!(any(target_os = "linux", windows))
}

/// Restores the affinity of the calling thread when dropped.
pub struct ThreadPin {
    #[cfg(target_os = "linux")]
    previous: libc::cpu_set_t,
    #[cfg(windows)]
    previous: usize,
}

/// Restrict the calling thread (and, on Linux, the threads and processes it
/// starts) to `cpus`. `None` if that is not supported or failed.
pub fn pin_current_thread(cpus: &[usize]) -> Option<ThreadPin> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: all-zero cpu_set_t values are empty sets; the libc calls only
        // read or write the given sets of the stated size.
        unsafe {
            let size = std::mem::size_of::<libc::cpu_set_t>();
            let mut previous: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, size, &mut previous) != 0 {
                return None;
            }
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            for &cpu in cpus {
                libc::CPU_SET(cpu, &mut set);
            }
            (libc::sched_setaffinity(0, size, &set) == 0).then_some(ThreadPin { previous })
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
        let mask = super::affinity::windows_mask(cpus);
        // SAFETY: the pseudo handle of the calling thread is always valid.
        let previous = unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) };
        (previous != 0).then_some(ThreadPin { previous })
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = cpus;
        None
    }
}

impl Drop for ThreadPin {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        // SAFETY: `previous` was filled by sched_getaffinity.
        unsafe {
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &self.previous);
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
            // SAFETY: the pseudo handle of the calling thread is always valid.
            unsafe { SetThreadAffinityMask(GetCurrentThread(), self.previous) };
        }
    }
}

/// `--check-system`: a note when workers share a physical core.
pub fn describe_sharing(partition: &Partition) -> Option<String> {
    partition.shared_siblings.then(|| {
        format!(
            "{} workers on fewer physical cores: SMT siblings are shared ({:?})",
            partition.workers.len(),
            partition.workers
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_smt(cpus: usize) -> Topology {
        Topology {
            allowed: (0..cpus).collect(),
            cores: Vec::new(),
        }
    }

    /// 2 hardware threads per core, siblings adjacent: (0,1) (2,3) ...
    fn smt(cpus: usize) -> Topology {
        Topology {
            allowed: (0..cpus).collect(),
            cores: (0..cpus).step_by(2).map(|c| vec![c, c + 1]).collect(),
        }
    }

    /// siblings far apart, as on many Intel CPUs: (0,4) (1,5) (2,6) (3,7)
    fn smt_split(cpus: usize) -> Topology {
        let half = cpus / 2;
        Topology {
            allowed: (0..cpus).collect(),
            cores: (0..half).map(|c| vec![c, c + half]).collect(),
        }
    }

    #[test]
    fn one_cpu_per_worker_without_smt() {
        let p = partition(&no_smt(8), None, 3).unwrap();
        assert_eq!(p.workers, [[0], [1], [2]]);
        assert!(p.reserved.is_empty() && !p.shared_siblings);
    }

    #[test]
    fn siblings_of_a_core_stay_together() {
        let p = partition(&smt(8), None, 2).unwrap();
        assert_eq!(p.workers, [[0, 1], [2, 3]]);
        assert!(!p.shared_siblings);
        let p = partition(&smt_split(8), None, 2).unwrap();
        assert_eq!(p.workers, [[0, 4], [1, 5]]);
    }

    #[test]
    fn more_workers_than_cores_share_siblings_and_say_so() {
        let p = partition(&smt(8), None, 6).unwrap();
        // one hardware thread each: the first threads of all four cores, then two second ones
        assert_eq!(p.workers, [[0], [2], [4], [6], [1], [3]]);
        assert!(p.shared_siblings);
        // exactly one worker per core is not shared
        assert!(!partition(&smt(8), None, 4).unwrap().shared_siblings);
    }

    #[test]
    fn worker_sets_are_disjoint_and_exclude_reserved_cpus() {
        for topology in [no_smt(16), smt(16), smt_split(16)] {
            for reserve in [
                None,
                Some(Reserve::Count(1)),
                Some(Reserve::Count(3)),
                Some(Reserve::List(vec![0, 15])),
            ] {
                for jobs in 1..=8 {
                    let Ok(p) = partition(&topology, reserve.as_ref(), jobs) else {
                        continue;
                    };
                    assert_eq!(p.workers.len(), jobs);
                    let mut seen = std::collections::BTreeSet::new();
                    for cpu in p.workers.iter().flatten() {
                        assert!(seen.insert(*cpu), "{cpu} twice in {p:?}");
                        assert!(!p.reserved.contains(cpu), "{cpu} reserved in {p:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_reserved_count_takes_the_highest_cpus_and_their_siblings() {
        let p = partition(&no_smt(16), Some(&Reserve::Count(1)), 2).unwrap();
        assert_eq!(p.reserved, [15]);
        assert_eq!(p.workers, [[0], [1]]);
        let p = partition(&smt(16), Some(&Reserve::Count(1)), 2).unwrap();
        assert_eq!(p.reserved, [14, 15], "the sibling of 15 is reserved too");
        let p = partition(&smt(16), Some(&Reserve::List(vec![15])), 2).unwrap();
        assert_eq!(p.reserved, [15], "an explicit list is used as given");
        assert_eq!(p.workers, [[0, 1], [2, 3]]);
    }

    #[test]
    fn a_restricted_affinity_limits_the_partition() {
        let topology = Topology {
            allowed: vec![2, 3, 6, 7],
            cores: vec![vec![2, 3], vec![6, 7]],
        };
        let p = partition(&topology, None, 2).unwrap();
        assert_eq!(p.workers, [[2, 3], [6, 7]]);
        let err = partition(&topology, None, 5).unwrap_err().to_string();
        assert!(err.contains("only 4 are usable"), "{err}");
    }

    #[test]
    fn fewer_cpus_than_jobs_is_a_clear_error() {
        let err = partition(&no_smt(4), None, 6).unwrap_err().to_string();
        assert!(err.contains("'--jobs 6' needs 6 CPUs, but only 4"), "{err}");
        let err = partition(&no_smt(4), Some(&Reserve::Count(2)), 3)
            .unwrap_err()
            .to_string();
        assert!(err.contains("after reserving"), "{err}");
    }

    #[test]
    fn invalid_reservations_are_errors() {
        let err = partition(&no_smt(4), Some(&Reserve::List(vec![9])), 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("CPU 9"), "{err}");
        assert!(partition(&no_smt(4), Some(&Reserve::Count(4)), 1).is_err());
    }

    #[test]
    fn parses_the_reserve_value() {
        assert_eq!(Reserve::parse("2").unwrap(), Reserve::Count(2));
        assert_eq!(Reserve::parse("0,3").unwrap(), Reserve::List(vec![0, 3]));
        assert_eq!(Reserve::parse("1-2").unwrap(), Reserve::List(vec![1, 2]));
        for bad in ["0", "", "x", "3-1"] {
            assert!(Reserve::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn json_shape() {
        let p = partition(&no_smt(4), Some(&Reserve::List(vec![3])), 2).unwrap();
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "workers": [[0], [1]], "reserved": [3] })
        );
        let p = partition(&no_smt(4), None, 2).unwrap();
        assert!(serde_json::to_value(&p).unwrap().get("reserved").is_none());
    }
}
