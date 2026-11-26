//! Process-tree cancellation and leftover-process detection.
//!
//! Every measured process is the leader of a process group on Unix and the
//! only member of a Job Object on Windows (`docs/specs/process-tree.md`). The
//! processes that are running are registered here, so that
//!
//! - Ctrl-C (and SIGTERM/SIGHUP) terminates the whole tree of every run in
//!   flight, not only the direct child ([`interrupt`], [`kill_all`]);
//! - when a run ends, descendants that outlived the measured command are
//!   found, killed and reported ([`Tracked::finish`]). They would keep using
//!   the CPU, memory and energy budget of the runs that follow.
//!
//! Descendants that left the group (`setsid`, double fork) are found on Linux
//! through `PR_SET_CHILD_SUBREAPER`: when their parent exits, the kernel hands
//! them to perfratio instead of to `init`. macOS has no subreaper, so only
//! descendants that stay in the group are detected there.

use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard};

#[derive(Default)]
struct State {
    /// Process group leaders (Unix) or Job Object handles (Windows) of the
    /// runs in flight
    active: BTreeSet<usize>,
    /// Processes being spawned: not registered yet
    spawning: usize,
}

static STATE: Mutex<State> = Mutex::new(State {
    active: BTreeSet::new(),
    spawning: 0,
});

fn lock() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Announces a process that is about to be spawned. While it exists, no sweep
/// for orphans runs, so a process that was just spawned and is not registered
/// yet is not taken for an orphan.
pub struct Spawning {
    registered: bool,
}

/// Call right before spawning a measured process.
pub fn before_spawn() -> Spawning {
    #[cfg(target_os = "linux")]
    enable_subreaper();
    lock().spawning += 1;
    Spawning { registered: false }
}

impl Spawning {
    /// Register the process (`key`: pid of the group leader, or the Job Object
    /// handle) that was spawned.
    pub fn spawned(mut self, key: usize) -> Tracked {
        let mut state = lock();
        state.spawning -= 1;
        state.active.insert(key);
        self.registered = true;
        Tracked {
            key,
            finished: false,
        }
    }
}

impl Drop for Spawning {
    fn drop(&mut self) {
        if !self.registered {
            // spawn failed
            let mut state = lock();
            state.spawning = state.spawning.saturating_sub(1);
        }
    }
}

/// A running measured process. If it is dropped without [`Tracked::finish`]
/// (an error path), its whole tree is killed.
pub struct Tracked {
    key: usize,
    finished: bool,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        let mut state = lock();
        state.active.remove(&self.key);
        if !self.finished {
            platform::kill(self.key);
        }
    }
}

/// First Ctrl-C: ask the tree of every run in flight to stop, as the terminal
/// would have done (SIGINT), and kill what is still there after a grace period.
pub fn interrupt() {
    let keys: Vec<usize> = lock().active.iter().copied().collect();
    for key in &keys {
        platform::interrupt(*key);
    }
    if keys.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let state = lock();
        for key in keys.iter().filter(|key| state.active.contains(key)) {
            platform::kill(*key);
        }
    });
}

/// Second Ctrl-C: kill the tree of every run in flight at once.
pub fn kill_all() {
    let state = lock();
    for key in &state.active {
        platform::kill(*key);
    }
}

impl Tracked {
    /// The process was reaped. Kill and reap the descendants it left behind and
    /// report whether there were any.
    ///
    /// `windows_active` is, on Windows, the number of processes still in the
    /// Job Object (`None` if it cannot be queried).
    pub fn finish(mut self, windows_active: Option<u32>) -> bool {
        self.finished = true;
        let key = self.key;
        // Unregister before looking for orphans, so that this run's own
        // process does not keep the sweep from running
        lock().active.remove(&key);
        let mut leftover = platform::kill_group_leftovers(key, windows_active);
        leftover |= sweep_orphans();
        leftover
    }
}

/// Kill and reap the orphans (descendants that left their group) that
/// perfratio inherited as a subreaper. Only done while no other measured
/// process is running or being spawned, so that the descendants of a run in
/// flight are not taken for leftovers.
fn sweep_orphans() -> bool {
    #[cfg(target_os = "linux")]
    {
        let state = lock();
        if !state.active.is_empty() || state.spawning > 0 {
            return false;
        }
        orphans::sweep()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

#[cfg(target_os = "linux")]
fn enable_subreaper() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: plain syscall without pointer arguments. If it fails, only
        // the detection of descendants that leave their group is lost.
        unsafe {
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        }
    });
}

#[cfg(unix)]
mod platform {
    pub fn signal_group(pgid: usize, signal: libc::c_int) -> std::io::Result<()> {
        if pgid <= 1 {
            return Ok(());
        }
        // SAFETY: plain syscall; a negative pid targets the process group
        // created with `command.process_group(0)`.
        let ret = unsafe { libc::kill(-(pgid as libc::pid_t), signal) };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    pub fn interrupt(key: usize) {
        let _ = signal_group(key, libc::SIGINT);
    }

    pub fn kill(key: usize) {
        let _ = signal_group(key, libc::SIGKILL);
    }

    /// Kill what is left in the group of the reaped leader `key`. A group
    /// lives as long as it has a member (its id is not reused before), so a
    /// successful signal means that a descendant outlived the leader.
    pub fn kill_group_leftovers(key: usize, _windows_active: Option<u32>) -> bool {
        match signal_group(key, libc::SIGKILL) {
            Ok(()) => true,
            Err(error) => error.raw_os_error() != Some(libc::ESRCH),
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;

    pub fn interrupt(key: usize) {
        kill(key);
    }

    pub fn kill(key: usize) {
        // SAFETY: `key` is the handle of a Job Object that is open while it is
        // registered (the registry is locked while a handle is closed).
        unsafe {
            TerminateJobObject(key as HANDLE, 1);
        }
    }

    pub fn kill_group_leftovers(key: usize, active: Option<u32>) -> bool {
        let leftover = active.is_some_and(|count| count > 0);
        if leftover {
            kill(key);
        }
        leftover
    }
}

/// Descendants that left their process group, found through the subreaper.
#[cfg(target_os = "linux")]
mod orphans {
    use std::time::{Duration, Instant};

    /// (state, parent, process group) of a process from `/proc/<pid>/stat`
    fn stat(pid: i32) -> Option<(char, i32, i32)> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_stat(&text)
    }

    /// `pid (comm) S ppid pgrp ...`: `comm` may contain spaces and parentheses.
    pub fn parse_stat(text: &str) -> Option<(char, i32, i32)> {
        let rest = &text[text.rfind(')')? + 1..];
        let mut fields = rest.split_whitespace();
        let state = fields.next()?.chars().next()?;
        let ppid = fields.next()?.parse().ok()?;
        let pgrp = fields.next()?.parse().ok()?;
        Some((state, ppid, pgrp))
    }

    /// The children of this process.
    fn children() -> Vec<i32> {
        let mut pids = Vec::new();
        let mut listed = false;
        if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
            for task in tasks.flatten() {
                let path = task.path().join("children");
                if let Ok(text) = std::fs::read_to_string(path) {
                    listed = true;
                    pids.extend(
                        text.split_whitespace()
                            .filter_map(|p| p.parse::<i32>().ok()),
                    );
                }
            }
        }
        if listed {
            return pids;
        }
        // Kernel without CONFIG_PROC_CHILDREN: look at every process
        let me = std::process::id() as i32;
        std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter(|pid| stat(*pid).is_some_and(|(_, ppid, _)| ppid == me))
            .collect()
    }

    /// Kill and reap every child of ours that is not in our own process group.
    /// Everything perfratio started to measure is in a group of its own, so
    /// such a child is a descendant of a measured command that was handed to
    /// us when its parent exited. Children in our group (helper processes of
    /// perfratio itself) are left alone.
    pub fn sweep() -> bool {
        // SAFETY: plain syscall.
        let own_group = unsafe { libc::getpgrp() };
        let mut found = false;
        for pid in children() {
            let Some((_, _, pgrp)) = stat(pid) else {
                continue;
            };
            if pgrp == own_group {
                continue;
            }
            found = true;
            // SAFETY: plain syscall on a child of this process.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
            reap(pid);
        }
        found
    }

    fn reap(pid: i32) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let mut status = 0;
            // SAFETY: `pid` is a child of this process and `status` is a valid out-pointer.
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if ret != 0 || Instant::now() > deadline {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn parses_proc_stat_with_awkward_names() {
        use super::orphans::parse_stat;
        assert_eq!(parse_stat("42 (sleep) S 1 42 42 0"), Some(('S', 1, 42)));
        assert_eq!(parse_stat("7 (a) b (c)) Z 3 9 9 0"), Some(('Z', 3, 9)));
        assert_eq!(parse_stat("garbage"), None);
    }
}
