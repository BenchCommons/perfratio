//! `--affinity`: pin benchmarked processes to a set of CPUs.

use anyhow::{bail, ensure, Context, Result};

/// Parse a CPU list such as `2`, `0-3` or `0,2,4-5` into sorted, unique CPU
/// indices.
pub fn parse_cpu_list(list: &str) -> Result<Vec<usize>> {
    let mut cpus = Vec::new();
    for part in list.split(',').map(str::trim) {
        ensure!(!part.is_empty(), "empty entry in CPU list '{list}'");
        match part.split_once('-') {
            Some((first, last)) => {
                let first: usize = first
                    .trim()
                    .parse()
                    .with_context(|| format!("invalid CPU '{first}' in '{list}'"))?;
                let last: usize = last
                    .trim()
                    .parse()
                    .with_context(|| format!("invalid CPU '{last}' in '{list}'"))?;
                ensure!(first <= last, "invalid CPU range '{part}' in '{list}'");
                cpus.extend(first..=last);
            }
            None => cpus.push(
                part.parse()
                    .with_context(|| format!("invalid CPU '{part}' in '{list}'"))?,
            ),
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    Ok(cpus)
}

/// Core class selected by `--affinity p-cores|e-cores` (macOS).
///
/// macOS cannot pin a process to CPUs. On Apple Silicon the scheduler picks the
/// core type from the task and thread policy, so this is a scheduling
/// preference, not a hard guarantee like Linux `sched_setaffinity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreClass {
    /// Performance cores (foreground task role)
    Performance,
    /// Efficiency cores (`PRIO_DARWIN_BG`)
    Efficiency,
}

impl CoreClass {
    /// Parse `p-cores` or `e-cores` (case-insensitive). `None` for anything else.
    pub fn parse(value: &str) -> Option<CoreClass> {
        match value.trim().to_ascii_lowercase().as_str() {
            "p-cores" => Some(CoreClass::Performance),
            "e-cores" => Some(CoreClass::Efficiency),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CoreClass::Performance => "p-cores",
            CoreClass::Efficiency => "e-cores",
        }
    }
}

/// Check a core class request. `perf_levels` is the number of CPU performance
/// levels the system reports (`hw.nperflevels` on macOS), `None` if unknown.
pub fn validate_core_class(class: CoreClass, perf_levels: Option<u32>) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!(
            "'--affinity {}' is only supported on macOS; use a CPU list such as '0-3' on \
             Linux and Windows",
            class.as_str()
        );
    }
    ensure!(
        perf_levels.is_some_and(|levels| levels > 1),
        "'--affinity {}' needs a CPU with performance and efficiency cores (Apple Silicon); \
         this system has a single core type",
        class.as_str()
    );
    Ok(())
}

/// Number of CPU performance levels (`hw.nperflevels`): 2 on Apple Silicon with
/// P and E cores, 1 on Intel Macs.
#[cfg(target_os = "macos")]
pub fn perf_levels() -> Option<u32> {
    let mut value: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is a NUL-terminated string and `value`/`size` describe a
    // valid c_int buffer.
    let ret = unsafe {
        libc::sysctlbyname(
            c"hw.nperflevels".as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (ret == 0 && value > 0).then_some(value as u32)
}

#[cfg(not(target_os = "macos"))]
pub fn perf_levels() -> Option<u32> {
    None
}

/// Mach task policy declarations (libSystem); the `libc` crate lacks them.
#[cfg(target_os = "macos")]
mod mach {
    pub const TASK_CATEGORY_POLICY: u32 = 1;
    pub const TASK_FOREGROUND_APPLICATION: i32 = 1;
    /// Size of `task_category_policy_data_t` in 32-bit words
    pub const TASK_CATEGORY_POLICY_COUNT: u32 = 1;

    extern "C" {
        pub static mach_task_self_: u32;
        pub fn task_policy_set(task: u32, flavor: u32, info: *mut i32, count: u32) -> i32;
    }
}

/// Give the calling task the foreground application role (macOS). Unlike
/// thread QoS and thread policies, the task role survives exec.
#[cfg(target_os = "macos")]
pub fn set_foreground_role() -> std::io::Result<()> {
    let mut role = mach::TASK_FOREGROUND_APPLICATION;
    // SAFETY: `role` is a task_category_policy_data_t of the stated size; after
    // fork, mach_task_self_ names the child's own task.
    let ret = unsafe {
        mach::task_policy_set(
            mach::mach_task_self_,
            mach::TASK_CATEGORY_POLICY,
            &mut role,
            mach::TASK_CATEGORY_POLICY_COUNT,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "task_policy_set(TASK_CATEGORY_POLICY) failed with kern_return_t {ret}"
        )))
    }
}

/// Prefer the given core class for the process spawned by `command` (macOS).
/// Set in the child between fork and exec with task-level policies, which
/// (unlike thread QoS) survive exec and are inherited by its children:
/// - e-cores: `PRIO_DARWIN_BG`; on Apple Silicon background tasks run on the
///   efficiency cores only (also lowers I/O priority).
/// - p-cores: clears `PRIO_DARWIN_BG` and sets the foreground task role. The
///   scheduler may still place threads on efficiency cores under load.
#[cfg(target_os = "macos")]
pub fn apply_core_class(command: &mut std::process::Command, class: CoreClass) {
    use std::os::unix::process::CommandExt;

    // SAFETY: the closure runs between fork and exec and only calls the
    // setpriority syscall and the task_policy_set Mach call on the child.
    unsafe {
        command.pre_exec(move || {
            let background = match class {
                CoreClass::Efficiency => libc::PRIO_DARWIN_BG,
                CoreClass::Performance => 0,
            };
            if libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, background) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if class == CoreClass::Performance {
                set_foreground_role()?;
            }
            Ok(())
        });
    }
}

/// Check that the CPUs exist and that pinning is supported on this platform.
pub fn validate(cpus: &[usize]) -> Result<()> {
    if cfg!(target_os = "macos") {
        bail!(
            "'--affinity' with a CPU list is not supported on macOS (it has no API to pin a \
             process to CPUs); use '--affinity p-cores' or '--affinity e-cores' to prefer a \
             core type"
        );
    }
    if !cfg!(any(target_os = "linux", windows)) {
        bail!(
            "'--affinity' is only supported on Linux, Windows and macOS (this OS has no API \
             to pin a process to CPUs)"
        );
    }
    let available = configured_cpus();
    if let Some(&cpu) = cpus.iter().find(|&&cpu| cpu >= available) {
        bail!(
            "CPU {cpu} does not exist: this system has CPUs 0-{}",
            available - 1
        );
    }
    if cfg!(windows) && cpus.iter().any(|&cpu| cpu >= usize::BITS as usize) {
        bail!(
            "'--affinity' supports CPUs 0-{} on Windows",
            usize::BITS - 1
        );
    }
    Ok(())
}

/// Number of CPUs configured in the system (not just the ones perfratio may use).
fn configured_cpus() -> usize {
    #[cfg(unix)]
    {
        // SAFETY: sysconf is always safe to call.
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
        if n > 0 {
            return n as usize;
        }
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Restrict the process spawned by `command` to `cpus` (Linux). The affinity
/// is set in the child between fork and exec, so the benchmarked program never
/// runs on other CPUs.
#[cfg(target_os = "linux")]
pub fn apply(command: &mut std::process::Command, cpus: &[usize]) {
    use std::os::unix::process::CommandExt;

    // SAFETY: an all-zero cpu_set_t is the empty set.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for &cpu in cpus {
        // SAFETY: CPU_SET only writes into `set`; `cpu` was validated.
        unsafe { libc::CPU_SET(cpu, &mut set) };
    }
    // SAFETY: the closure runs between fork and exec and only calls the
    // async-signal-safe sched_setaffinity syscall on data captured by value.
    unsafe {
        command.pre_exec(move || {
            if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Affinity mask for `SetProcessAffinityMask` (Windows).
#[cfg(windows)]
pub fn windows_mask(cpus: &[usize]) -> usize {
    cpus.iter().fold(0, |mask, &cpu| mask | (1usize << cpu))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_cpu_list("0").unwrap(), vec![0]);
        assert_eq!(parse_cpu_list("0-3").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_cpu_list("5, 1,3-4").unwrap(), vec![1, 3, 4, 5]);
        assert_eq!(parse_cpu_list("2,2,1-2").unwrap(), vec![1, 2]);
    }

    #[test]
    fn rejects_invalid_cpu_lists() {
        for list in ["", "a", "3-1", "1,", "-2", "1-x"] {
            assert!(parse_cpu_list(list).is_err(), "{list}");
        }
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn rejects_cpus_that_do_not_exist() {
        let err = validate(&[100_000]).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
        assert!(validate(&[0]).is_ok());
    }

    #[test]
    fn parses_core_classes() {
        assert_eq!(CoreClass::parse("p-cores"), Some(CoreClass::Performance));
        assert_eq!(CoreClass::parse(" E-Cores "), Some(CoreClass::Efficiency));
        assert_eq!(CoreClass::parse("0-3"), None);
        assert_eq!(CoreClass::parse("cores"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn core_class_needs_two_performance_levels() {
        assert!(validate_core_class(CoreClass::Efficiency, Some(2)).is_ok());
        for levels in [None, Some(1)] {
            let err = validate_core_class(CoreClass::Performance, levels).unwrap_err();
            assert!(err.to_string().contains("single core type"), "{err}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn core_class_is_macos_only() {
        let err = validate_core_class(CoreClass::Performance, Some(2)).unwrap_err();
        assert!(err.to_string().contains("only supported on macOS"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cpu_list_is_rejected_on_macos_with_alternative() {
        let err = validate(&[0]).unwrap_err().to_string();
        assert!(err.contains("p-cores") && err.contains("not supported on macOS"));
    }

    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    #[test]
    fn is_unsupported_on_this_platform() {
        assert!(validate(&[0])
            .unwrap_err()
            .to_string()
            .contains("only supported"));
    }
}
