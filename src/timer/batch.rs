//! Bookkeeping for the instances of a `--concurrency N` batch.
//!
//! Every instance runs `execute_and_measure` on its own thread. When one
//! instance fails to spawn, the instances of the same run that already started
//! are killed instead of being waited for (docs/specs/concurrency.md).
//!
//! The registry is reached through a thread-local so that the single-instance
//! path (no registry installed) is unchanged.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    /// Processes started by the batch and not reaped yet
    running: BTreeSet<u32>,
    /// Set by the first spawn failure: the message of its error
    aborted: Option<String>,
    /// Instances killed because of the abort
    killed: usize,
}

/// Shared by the threads of one batch.
#[derive(Clone, Default)]
pub struct BatchRegistry {
    state: Arc<Mutex<State>>,
}

thread_local! {
    static CURRENT: RefCell<Option<BatchRegistry>> = const { RefCell::new(None) };
}

/// Install `registry` for the current thread. The returned guard removes it.
pub fn install(registry: &BatchRegistry) -> InstallGuard {
    CURRENT.with(|current| *current.borrow_mut() = Some(registry.clone()));
    InstallGuard
}

pub struct InstallGuard;

impl Drop for InstallGuard {
    fn drop(&mut self) {
        CURRENT.with(|current| *current.borrow_mut() = None);
    }
}

impl BatchRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The message of the spawn failure that aborted the batch, if any, and
    /// the number of instances that were killed because of it.
    pub fn abort_reason(&self) -> Option<(String, usize)> {
        let state = self.lock();
        state.aborted.clone().map(|msg| (msg, state.killed))
    }
}

/// Whether the batch of the current thread was aborted by a spawn failure.
/// Checked right before spawning, so that instances that did not start yet
/// are not started.
pub fn aborted() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|registry| registry.lock().aborted.is_some())
    })
}

/// An instance failed to spawn: kill the started instances of the batch.
/// Does nothing outside of a batch.
pub fn spawn_failed(error: &std::io::Error) {
    CURRENT.with(|current| {
        let Some(registry) = current.borrow().clone() else {
            return;
        };
        let mut state = registry.lock();
        if state.aborted.is_some() {
            return;
        }
        state.aborted = Some(error.to_string());
        let pids: Vec<u32> = state.running.iter().copied().collect();
        for pid in &pids {
            kill(*pid);
        }
        state.killed = pids.len();
    });
}

/// Registers a started process; unregisters it when dropped (after the
/// process was reaped, so the pid is not reused while it is registered).
pub struct SpawnedGuard {
    registry: Option<BatchRegistry>,
    pid: u32,
}

/// Register the process `pid` that was just spawned. When the batch was
/// aborted in the meantime, the process is killed at once.
pub fn spawned(pid: u32) -> SpawnedGuard {
    let registry = CURRENT.with(|current| current.borrow().clone());
    if let Some(registry) = &registry {
        let mut state = registry.lock();
        if state.aborted.is_some() {
            kill(pid);
            state.killed += 1;
        } else {
            state.running.insert(pid);
        }
    }
    SpawnedGuard { registry, pid }
}

impl Drop for SpawnedGuard {
    fn drop(&mut self) {
        if let Some(registry) = &self.registry {
            registry.lock().running.remove(&self.pid);
        }
    }
}

#[cfg(unix)]
fn kill(pid: u32) {
    // SAFETY: plain syscalls. The process group exists only when a timeout or
    // `--until` created one; the second call covers the other case.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: the handle is opened, used and closed here.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    #[test]
    fn spawn_failure_kills_started_instances() {
        let registry = BatchRegistry::default();
        let _installed = install(&registry);
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let _guard = spawned(child.id());
        assert!(!aborted());

        spawn_failed(&std::io::Error::from_raw_os_error(libc::EAGAIN));

        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(aborted());
        let (_, killed) = registry.abort_reason().unwrap();
        assert_eq!(killed, 1);
    }

    #[test]
    fn instance_registered_after_the_abort_is_killed() {
        let registry = BatchRegistry::default();
        let _installed = install(&registry);
        spawn_failed(&std::io::Error::from_raw_os_error(libc::EAGAIN));
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let _guard = spawned(child.id());
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn finished_instances_are_not_killed() {
        let registry = BatchRegistry::default();
        let _installed = install(&registry);
        let mut child = Command::new("true").spawn().unwrap();
        drop(spawned(child.id()));
        child.wait().unwrap();
        spawn_failed(&std::io::Error::from_raw_os_error(libc::EAGAIN));
        assert_eq!(registry.abort_reason().unwrap().1, 0);
    }

    #[test]
    fn without_registry_nothing_happens() {
        assert!(!aborted());
        spawn_failed(&std::io::Error::from_raw_os_error(libc::EAGAIN));
        drop(spawned(1));
    }
}
