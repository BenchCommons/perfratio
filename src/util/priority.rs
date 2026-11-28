//! `--priority`: run benchmarked processes with a different scheduling
//! priority, to reduce preemption by other processes (`realtime`, `high`) or to
//! benchmark background workloads (`idle`).

use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    /// Inherit perfratio's priority (default)
    #[default]
    Normal,
    /// Linux/macOS: nice -20. Windows: HIGH_PRIORITY_CLASS
    High,
    /// Linux: SCHED_IDLE. macOS: nice 19. Windows: IDLE_PRIORITY_CLASS
    Idle,
    /// Linux: SCHED_FIFO at the maximum priority. Windows:
    /// REALTIME_PRIORITY_CLASS. macOS: nice -20 plus the foreground task
    /// role (not equivalent to SCHED_FIFO, see docs/specs/priority.md).
    Realtime,
}

impl Priority {
    pub fn parse(value: &str) -> Result<Self> {
        let priority = match value {
            "normal" => Priority::Normal,
            "high" => Priority::High,
            "idle" => Priority::Idle,
            "realtime" => Priority::Realtime,
            _ => bail!("invalid priority '{value}' (normal, high, idle, realtime)"),
        };
        if priority == Priority::Realtime
            && !cfg!(any(target_os = "linux", windows, target_os = "macos"))
        {
            bail!(
                "'--priority realtime' is only supported on Linux, macOS and Windows \
                 (use 'high' for the highest nice value)"
            );
        }
        Ok(priority)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Normal => "normal",
            Priority::High => "high",
            Priority::Idle => "idle",
            Priority::Realtime => "realtime",
        }
    }
}

/// How to get the permission that `priority` needs, if it failed.
pub fn permission_hint(priority: Priority) -> Option<&'static str> {
    match priority {
        Priority::Normal | Priority::Idle => None,
        Priority::High | Priority::Realtime => Some(if cfg!(target_os = "linux") {
            "raising the priority requires the CAP_SYS_NICE capability: run perfratio as root, \
             or grant it with 'sudo setcap cap_sys_nice+ep \"$(command -v perfratio)\"'"
        } else if cfg!(windows) {
            "without administrator rights, Windows silently lowers 'realtime' to 'high'"
        } else {
            "raising the priority requires root: run perfratio with sudo"
        }),
    }
}

/// Unix: set the priority in the child between fork and exec, so the
/// benchmarked program (and everything it starts) never runs with the old one.
#[cfg(unix)]
pub fn apply(command: &mut std::process::Command, priority: Priority) {
    use std::os::unix::process::CommandExt;

    if priority == Priority::Normal {
        return;
    }
    // SAFETY: the closure runs between fork and exec and only calls the
    // async-signal-safe setpriority / sched_setscheduler syscalls (on macOS also
    // task_policy_set) on values captured by copy.
    unsafe {
        command.pre_exec(move || set_current_process_priority(priority));
    }
}

#[cfg(unix)]
fn set_current_process_priority(priority: Priority) -> std::io::Result<()> {
    let check = |ret: libc::c_int| {
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    // `setpriority`'s `which` argument has a different integer type on glibc
    // than on other libcs, hence the inferred casts below.

    match priority {
        Priority::Normal => Ok(()),
        // SAFETY: plain syscall on the calling (child) process.
        Priority::High => check(unsafe { libc::setpriority(libc::PRIO_PROCESS as _, 0, -20) }),
        #[cfg(target_os = "linux")]
        Priority::Idle => {
            // SAFETY: zeroed sched_param struct is valid across all libcs (glibc, musl).
            let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
            param.sched_priority = 0;
            // NOTE: musl libc's sched_setscheduler wrapper intentionally returns ENOSYS (os error 38)
            // due to POSIX conformance (POSIX specifies process scheduling, while Linux implements
            // thread scheduling). Calling the Linux syscall directly works on both glibc and musl.
            // SAFETY: SYS_sched_setscheduler is a standard Linux syscall on pid 0 (calling process).
            let ret = unsafe {
                libc::syscall(
                    libc::SYS_sched_setscheduler,
                    0 as libc::pid_t,
                    libc::SCHED_IDLE as libc::c_int,
                    &param,
                )
            };
            check(ret as libc::c_int)
        }
        #[cfg(not(target_os = "linux"))]
        // SAFETY: plain syscall on the calling (child) process.
        Priority::Idle => check(unsafe { libc::setpriority(libc::PRIO_PROCESS as _, 0, 19) }),
        #[cfg(target_os = "linux")]
        Priority::Realtime => {
            // SAFETY: plain syscall without side effects.
            let max = unsafe { libc::sched_get_priority_max(libc::SCHED_FIFO) };
            // SAFETY: zeroed sched_param struct is valid across all libcs (glibc, musl).
            let mut param: libc::sched_param = unsafe { std::mem::zeroed() };
            param.sched_priority = max;
            // SAFETY: SYS_sched_setscheduler is a standard Linux syscall on pid 0 (calling process).
            let ret = unsafe {
                libc::syscall(
                    libc::SYS_sched_setscheduler,
                    0 as libc::pid_t,
                    libc::SCHED_FIFO as libc::c_int,
                    &param,
                )
            };
            check(ret as libc::c_int)
        }
        #[cfg(target_os = "macos")]
        Priority::Realtime => {
            // Mach thread policies (THREAD_TIME_CONSTRAINT_POLICY, thread QoS)
            // are reset by exec, so use the strongest task-level settings that
            // survive it: nice -20 (needs root) and the foreground task role.
            // SAFETY: plain syscall on the calling (child) process.
            check(unsafe { libc::setpriority(libc::PRIO_PROCESS as _, 0, -20) })?;
            crate::util::affinity::set_foreground_role()
        }
        // Rejected by `Priority::parse` on other Unix systems
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Priority::Realtime => Err(std::io::Error::from(std::io::ErrorKind::Unsupported)),
    }
}

/// Windows priority class for `SetPriorityClass`, `None` to keep the default.
#[cfg(windows)]
pub fn windows_priority_class(priority: Priority) -> Option<u32> {
    use windows_sys::Win32::System::Threading::{
        HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, REALTIME_PRIORITY_CLASS,
    };
    match priority {
        Priority::Normal => None,
        Priority::High => Some(HIGH_PRIORITY_CLASS),
        Priority::Idle => Some(IDLE_PRIORITY_CLASS),
        Priority::Realtime => Some(REALTIME_PRIORITY_CLASS),
    }
}

/// Windows priority class (`GetPriorityClass` value) that `priority` requests.
/// Plain constants, so the comparison below is testable on every platform.
const WIN_IDLE: u32 = 0x0000_0040;
const WIN_HIGH: u32 = 0x0000_0080;
const WIN_REALTIME: u32 = 0x0000_0100;

fn windows_class_name(class: u32) -> Option<&'static str> {
    match class {
        WIN_IDLE => Some("IDLE_PRIORITY_CLASS"),
        WIN_HIGH => Some("HIGH_PRIORITY_CLASS"),
        WIN_REALTIME => Some("REALTIME_PRIORITY_CLASS"),
        _ => None,
    }
}

/// Warning text when the class read back with `GetPriorityClass` differs from
/// the one requested with `SetPriorityClass` (Windows lowers REALTIME to HIGH
/// without administrator rights and reports success). `None` when they match.
pub fn priority_downgrade_warning(
    priority: Priority,
    requested: u32,
    effective: u32,
) -> Option<String> {
    if requested == effective {
        return None;
    }
    let name = |class: u32| {
        windows_class_name(class).map_or_else(|| format!("class {class:#x}"), str::to_string)
    };
    let mut message = format!(
        "'--priority {}' requested {} but Windows applied {}",
        priority.as_str(),
        name(requested),
        name(effective)
    );
    if requested == WIN_REALTIME && effective == WIN_HIGH {
        message.push_str(": REALTIME needs administrator rights, run perfratio elevated");
    }
    Some(message)
}

/// Windows: read the class back after `SetPriorityClass` and warn (once per
/// process) if it is not the requested one. NOT TESTED LOCALLY: needs Windows.
#[cfg(windows)]
pub fn warn_if_downgraded(child: &std::process::Child, priority: Priority, requested: u32) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::GetPriorityClass;
    static ONCE: std::sync::Once = std::sync::Once::new();

    // SAFETY: the handle belongs to our own child process, which is still alive
    // (suspended) at this point.
    let effective = unsafe { GetPriorityClass(child.as_raw_handle() as HANDLE) };
    if effective == 0 {
        let error = std::io::Error::last_os_error();
        ONCE.call_once(|| {
            eprintln!("Warning: could not read back the priority class to verify it: {error}")
        });
        return;
    }
    if let Some(message) = priority_downgrade_warning(priority, requested, effective) {
        ONCE.call_once(|| eprintln!("Warning: {message}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_priorities() {
        assert_eq!(Priority::parse("normal").unwrap(), Priority::Normal);
        assert_eq!(Priority::parse("high").unwrap(), Priority::High);
        assert_eq!(Priority::parse("idle").unwrap(), Priority::Idle);
        assert!(Priority::parse("fast").is_err());
        assert_eq!(
            Priority::parse("realtime").is_ok(),
            cfg!(any(target_os = "linux", windows, target_os = "macos"))
        );
        for p in [Priority::Normal, Priority::High, Priority::Idle] {
            assert_eq!(Priority::parse(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn downgrade_warning_only_when_class_differs() {
        let p = Priority::Realtime;
        assert!(priority_downgrade_warning(p, WIN_REALTIME, WIN_REALTIME).is_none());
        let w = priority_downgrade_warning(p, WIN_REALTIME, WIN_HIGH).unwrap();
        assert!(w.contains("REALTIME_PRIORITY_CLASS") && w.contains("HIGH_PRIORITY_CLASS"));
        assert!(w.contains("administrator"));
        let w = priority_downgrade_warning(Priority::High, WIN_HIGH, 0x20).unwrap();
        assert!(w.contains("class 0x20") && !w.contains("administrator"));
    }

    #[cfg(windows)]
    #[test]
    fn class_constants_match_windows_sys() {
        use windows_sys::Win32::System::Threading::*;
        assert_eq!(WIN_IDLE, IDLE_PRIORITY_CLASS);
        assert_eq!(WIN_HIGH, HIGH_PRIORITY_CLASS);
        assert_eq!(WIN_REALTIME, REALTIME_PRIORITY_CLASS);
    }

    #[test]
    fn only_raising_the_priority_needs_a_hint() {
        assert!(permission_hint(Priority::Normal).is_none());
        assert!(permission_hint(Priority::Idle).is_none());
        assert!(permission_hint(Priority::High).is_some());
        assert!(permission_hint(Priority::Realtime).is_some());
    }
}
