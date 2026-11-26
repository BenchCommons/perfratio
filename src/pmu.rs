//! Preflight for Linux hardware performance counters (`perf_event_open`):
//! `kernel.perf_event_paranoid`, permission errors (kernel setting, container
//! seccomp profile, missing capability) and machines without a PMU (VMs).
//! The decision logic is pure; only `probe` talks to the kernel (Linux only).

/// Highest `perf_event_paranoid` value that still lets an unprivileged process
/// count user-space events of itself and its children.
pub const MAX_PARANOID_UNPRIVILEGED: i32 = 2;

/// Result of the single `perf_event_open` probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Opened,
    /// The call failed with this `errno`.
    Failed(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PmuStatus {
    Available {
        paranoid: Option<i32>,
    },
    /// `EACCES` or `EPERM`
    Denied {
        paranoid: Option<i32>,
    },
    /// No usable PMU (typical in VMs) or no `perf_event_open` support
    Unsupported,
    /// The probe failed for another reason
    Other {
        errno: i32,
    },
    /// Not probed (no probe available); only the sysctl is known
    Unknown {
        paranoid: Option<i32>,
    },
}

/// Parse the content of `/proc/sys/kernel/perf_event_paranoid`.
pub fn parse_paranoid(content: &str) -> Option<i32> {
    content.trim().parse().ok()
}

/// Combine the sysctl value and the probe result. Without a probe, the sysctl
/// alone only tells whether unprivileged use is blocked.
pub fn decide(paranoid: Option<i32>, probe: Option<Probe>) -> PmuStatus {
    match probe {
        Some(Probe::Opened) => PmuStatus::Available { paranoid },
        Some(Probe::Failed(e)) if e == EACCES || e == EPERM => PmuStatus::Denied { paranoid },
        Some(Probe::Failed(e)) if [ENOENT, ENODEV, EOPNOTSUPP, ENOSYS].contains(&e) => {
            PmuStatus::Unsupported
        }
        Some(Probe::Failed(errno)) => PmuStatus::Other { errno },
        None => PmuStatus::Unknown { paranoid },
    }
}

// Linux errno values (identical on all Linux architectures used here).
const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EACCES: i32 = 13;
const ENODEV: i32 = 19;
const ENOSYS: i32 = 38;
const EOPNOTSUPP: i32 = 95;

/// Short summary for `--check-system`.
pub fn summary(status: &PmuStatus) -> String {
    let paranoid = |p: &Option<i32>| match p {
        Some(p) => format!("perf_event_paranoid={p}"),
        None => "perf_event_paranoid unknown".to_string(),
    };
    match status {
        PmuStatus::Available { paranoid: p } => format!("available ({})", paranoid(p)),
        PmuStatus::Denied { paranoid: p } => format!("permission denied ({})", paranoid(p)),
        PmuStatus::Unsupported => "no PMU available (virtual machine?)".to_string(),
        PmuStatus::Other { errno } => format!("perf_event_open failed (errno {errno})"),
        PmuStatus::Unknown { paranoid: p } if p.is_some_and(|p| p > MAX_PARANOID_UNPRIVILEGED) => {
            format!("permission denied ({})", paranoid(p))
        }
        PmuStatus::Unknown { paranoid: p } => format!("not probed ({})", paranoid(p)),
    }
}

/// Whether the status needs the user's attention.
pub fn is_problem(status: &PmuStatus) -> bool {
    match status {
        PmuStatus::Available { .. } => false,
        PmuStatus::Unknown { paranoid } => paranoid.is_some_and(|p| p > MAX_PARANOID_UNPRIVILEGED),
        _ => true,
    }
}

/// One-line remediation for `--check-system`.
pub fn hint(status: &PmuStatus) -> Option<String> {
    if !is_problem(status) {
        return None;
    }
    Some(match status {
        PmuStatus::Unsupported => {
            "hardware counters need a PMU; enable PMU/vPMU passthrough in the hypervisor".into()
        }
        PmuStatus::Other { .. } => "see `man 2 perf_event_open` for the errno".into(),
        PmuStatus::Denied {
            paranoid: Some(p), ..
        }
        | PmuStatus::Unknown {
            paranoid: Some(p), ..
        } if *p > MAX_PARANOID_UNPRIVILEGED => {
            "sudo sysctl kernel.perf_event_paranoid=2 (or lower)".into()
        }
        _ => {
            "grant CAP_PERFMON, or allow perf_event_open in the container's seccomp profile".into()
        }
    })
}

/// Multi-line, actionable error text for a feature that needs hardware
/// counters (for example `--counters`). `None` when counters are usable.
pub fn diagnostic(status: &PmuStatus, program: &str) -> Option<String> {
    if !is_problem(status) {
        return None;
    }
    let mut out = String::new();
    match status {
        PmuStatus::Unsupported => {
            out.push_str(
                "Hardware performance counters are not available on this machine \
                 (no PMU, or perf_event_open is unsupported).\n\
                 In a virtual machine, enable PMU (vPMU) passthrough in the hypervisor, \
                 for example `cpu mode='host-passthrough'` with libvirt.\n",
            );
        }
        PmuStatus::Other { errno } => {
            out.push_str(&format!(
                "Opening a hardware performance counter failed (errno {errno}). \
                 See `man 2 perf_event_open`.\n"
            ));
        }
        PmuStatus::Denied { paranoid } | PmuStatus::Unknown { paranoid } => {
            out.push_str("Access to Linux hardware performance counters was denied.\n");
            match paranoid {
                Some(p) if *p > MAX_PARANOID_UNPRIVILEGED => {
                    out.push_str(&format!(
                        "Current kernel.perf_event_paranoid setting: {p} \
                         (unprivileged use needs <= {MAX_PARANOID_UNPRIVILEGED}).\n\n\
                         To allow access without root:\n  \
                         sudo sysctl kernel.perf_event_paranoid={MAX_PARANOID_UNPRIVILEGED}\n\n\
                         To persist across reboots:\n  \
                         echo 'kernel.perf_event_paranoid = {MAX_PARANOID_UNPRIVILEGED}' | \
                         sudo tee /etc/sysctl.d/99-perf.conf\n\n"
                    ));
                }
                Some(p) => {
                    out.push_str(&format!(
                        "kernel.perf_event_paranoid is {p}, which allows it, so the block \
                         comes from elsewhere (a container seccomp profile or a missing \
                         capability).\n\n"
                    ));
                }
                None => out.push_str(
                    "kernel.perf_event_paranoid could not be read (/proc not available?).\n\n",
                ),
            }
            out.push_str(&format!(
                "Alternatively, grant the binary CAP_PERFMON:\n  \
                 sudo setcap cap_perfmon+ep $(which {program})\n\n\
                 In a container, allow the perf_event_open syscall in the seccomp profile \
                 and add CAP_PERFMON (Docker: --cap-add=PERFMON, plus a profile that \
                 allows perf_event_open).\n"
            ));
        }
        PmuStatus::Available { .. } => return None,
    }
    Some(out)
}

/// Open one non-destructive counter (user-space instructions of this process)
/// and close it again. Linux only.
#[cfg(target_os = "linux")]
pub fn probe() -> Probe {
    // struct perf_event_attr, zero-filled: type, size (u32 each), config,
    // sample_period, sample_type, read_format, then the flag bit field.
    let mut attr = [0u64; 16];
    attr[0] = u64::from(0u32) | (u64::from(std::mem::size_of_val(&attr) as u32) << 32); // PERF_TYPE_HARDWARE
    attr[1] = 1; // PERF_COUNT_HW_INSTRUCTIONS
    attr[5] = 1 | (1 << 5) | (1 << 6); // disabled, exclude_kernel, exclude_hv
    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;
    // SAFETY: `attr` is a valid, sufficiently large perf_event_attr; the
    // arguments are plain integers. The returned fd is closed right away.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            attr.as_ptr(),
            0 as libc::pid_t,
            -1 as libc::c_int,
            -1 as libc::c_int,
            PERF_FLAG_FD_CLOEXEC,
        )
    };
    if fd >= 0 {
        // SAFETY: `fd` was just returned by the kernel and is owned here.
        unsafe { libc::close(fd as libc::c_int) };
        Probe::Opened
    } else {
        Probe::Failed(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_paranoid() {
        assert_eq!(parse_paranoid("2\n"), Some(2));
        assert_eq!(parse_paranoid("-1"), Some(-1));
        assert_eq!(parse_paranoid(" 4 "), Some(4));
        assert_eq!(parse_paranoid(""), None);
        assert_eq!(parse_paranoid("x"), None);
    }

    #[test]
    fn decides_from_probe() {
        assert_eq!(
            decide(Some(1), Some(Probe::Opened)),
            PmuStatus::Available { paranoid: Some(1) }
        );
        for errno in [EACCES, EPERM] {
            assert_eq!(
                decide(Some(3), Some(Probe::Failed(errno))),
                PmuStatus::Denied { paranoid: Some(3) }
            );
        }
        for errno in [ENOENT, ENODEV, EOPNOTSUPP, ENOSYS] {
            assert_eq!(
                decide(Some(1), Some(Probe::Failed(errno))),
                PmuStatus::Unsupported
            );
        }
        assert_eq!(
            decide(None, Some(Probe::Failed(22))),
            PmuStatus::Other { errno: 22 }
        );
    }

    #[test]
    fn without_probe_only_sysctl_counts() {
        let high = decide(Some(3), None);
        assert!(is_problem(&high));
        assert!(summary(&high).starts_with("permission denied"));
        assert!(hint(&high).unwrap().contains("perf_event_paranoid=2"));
        assert!(!is_problem(&decide(Some(2), None)));
        assert!(!is_problem(&decide(None, None)));
        assert!(hint(&decide(Some(1), None)).is_none());
    }

    #[test]
    fn diagnostics_are_actionable() {
        let sysctl = diagnostic(&decide(Some(3), Some(Probe::Failed(EACCES))), "perfratio");
        let sysctl = sysctl.unwrap();
        assert!(sysctl.contains("setting: 3"));
        assert!(sysctl.contains("sudo sysctl kernel.perf_event_paranoid=2"));
        assert!(sysctl.contains("/etc/sysctl.d/99-perf.conf"));
        assert!(sysctl.contains("setcap cap_perfmon+ep $(which perfratio)"));

        let container = diagnostic(&decide(Some(2), Some(Probe::Failed(EPERM))), "perfratio");
        let container = container.unwrap();
        assert!(container.contains("seccomp"));
        assert!(!container.contains("sudo sysctl"));

        let vm = diagnostic(&decide(Some(2), Some(Probe::Failed(ENOENT))), "perfratio");
        assert!(vm.unwrap().contains("vPMU"));

        assert!(diagnostic(&decide(Some(1), Some(Probe::Opened)), "perfratio").is_none());
    }

    #[test]
    fn summaries() {
        assert_eq!(
            summary(&decide(Some(1), Some(Probe::Opened))),
            "available (perf_event_paranoid=1)"
        );
        assert_eq!(
            summary(&decide(None, Some(Probe::Failed(ENODEV)))),
            "no PMU available (virtual machine?)"
        );
        assert_eq!(
            summary(&decide(Some(2), None)),
            "not probed (perf_event_paranoid=2)"
        );
    }
}
