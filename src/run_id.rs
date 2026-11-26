//! Unique run identity (`PERFRATIO_RUN_ID`, `PERFRATIO_WORKER_ID`).
//!
//! Every run gets an id from one process-wide counter that starts at 0, so ids
//! are unique across warmup and timed runs, across commands and across the
//! instances of a `--concurrency` batch. The `--prepare`, the command and the
//! `--conclude` of one run see the same id. See `docs/specs/run-identity.md`.
//!
//! The runner opens a [`RunScope`] around one run; the executor reads the
//! identity from it when it starts a process. A thread-local carries the
//! scope, so the `Executor` interface stays unchanged. The instances of a
//! batch run on their own threads and install their id with [`enter_worker`].

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A block of consecutive run ids: one per instance of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunIds {
    pub first: u64,
    pub count: usize,
}

impl RunIds {
    /// The id of instance `worker` (0-based).
    pub fn id_of(&self, worker: usize) -> u64 {
        self.first + worker as u64
    }

    /// All ids of the block, in instance order.
    pub fn all(&self) -> Vec<u64> {
        (0..self.count).map(|worker| self.id_of(worker)).collect()
    }
}

/// Reserve `count` (at least 1) consecutive ids.
pub fn allocate(count: usize) -> RunIds {
    let count = count.max(1);
    let first = NEXT.fetch_add(count as u64, Ordering::Relaxed);
    RunIds { first, count }
}

thread_local! {
    static SCOPE: Cell<Option<RunIds>> = const { Cell::new(None) };
    static WORKER: Cell<Option<(u64, usize)>> = const { Cell::new(None) };
}

/// Marks the run of the current thread; the scope ends when dropped.
pub struct RunScope {
    ids: RunIds,
    previous: Option<RunIds>,
}

impl RunScope {
    pub fn ids(&self) -> RunIds {
        self.ids
    }
}

impl Drop for RunScope {
    fn drop(&mut self) {
        SCOPE.with(|scope| scope.set(self.previous));
    }
}

/// Start a run of `instances` instances (1 without `--concurrency`): allocate
/// its ids and make them the identity of everything started on this thread
/// until the returned scope is dropped.
pub fn enter(instances: usize) -> RunScope {
    let ids = allocate(instances);
    let previous = SCOPE.with(|scope| scope.replace(Some(ids)));
    RunScope { ids, previous }
}

/// The id the calibration runs (shell spawning time, counter overhead) use.
/// They are not runs of the benchmark, so they do not take ids from the counter.
pub const CALIBRATION_ID: u64 = u64::MAX;

/// Scope for calibration runs: `PERFRATIO_RUN_ID` is [`CALIBRATION_ID`].
pub fn enter_calibration() -> RunScope {
    let ids = RunIds {
        first: CALIBRATION_ID,
        count: 1,
    };
    let previous = SCOPE.with(|scope| scope.replace(Some(ids)));
    RunScope { ids, previous }
}

/// The ids of the batch the current thread is about to start: those of the
/// open scope, or a fresh block when no scope of this size is open.
pub fn batch_ids(instances: usize) -> RunIds {
    match SCOPE.with(Cell::get) {
        Some(ids) if ids.count == instances.max(1) => ids,
        _ => allocate(instances),
    }
}

/// Makes the current thread instance `worker` of a batch; ends when dropped.
pub struct WorkerGuard {
    previous: Option<(u64, usize)>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        WORKER.with(|worker| worker.set(self.previous));
    }
}

pub fn enter_worker(ids: RunIds, worker: usize) -> WorkerGuard {
    let previous = WORKER.with(|slot| slot.replace(Some((ids.id_of(worker), worker))));
    WorkerGuard { previous }
}

/// Identity of one process to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    pub run_id: u64,
    /// Position in a `--concurrency` batch; `None` outside of one
    pub worker_id: Option<usize>,
}

/// The identity for a process started on the current thread: the instance of
/// the batch it runs for, else the open scope, else a fresh id (for commands
/// outside of any run, e.g. calibration).
pub fn identity() -> Identity {
    if let Some((run_id, worker)) = WORKER.with(Cell::get) {
        return Identity {
            run_id,
            worker_id: Some(worker),
        };
    }
    let run_id = match SCOPE.with(Cell::get) {
        Some(ids) => ids.first,
        None => allocate(1).first,
    };
    Identity {
        run_id,
        worker_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_consecutive_and_unique() {
        let a = allocate(3);
        let b = allocate(1);
        assert_eq!(a.count, 3);
        assert_eq!(a.all(), vec![a.first, a.first + 1, a.first + 2]);
        assert_eq!(b.first, a.first + 3);
        assert_eq!(allocate(0).count, 1);
    }

    #[test]
    fn scope_shares_one_id_and_restores_the_previous_one() {
        assert_eq!(WORKER.with(Cell::get), None);
        let outer = enter(1);
        let id = identity();
        assert_eq!(id.run_id, outer.ids().first);
        assert_eq!(id.worker_id, None);
        // prepare, command and conclude of one run see the same id
        assert_eq!(identity(), id);
        {
            let inner = enter(1);
            assert_ne!(identity().run_id, id.run_id);
            assert_eq!(identity().run_id, inner.ids().first);
        }
        assert_eq!(identity(), id);
        drop(outer);
        // outside of a scope every call is a fresh id
        assert_ne!(identity().run_id, identity().run_id);
    }

    #[test]
    fn workers_get_distinct_ids_from_the_batch() {
        let scope = enter(4);
        let ids = batch_ids(4);
        assert_eq!(ids, scope.ids());
        let seen: std::collections::BTreeSet<u64> = std::thread::scope(|s| {
            (0..4)
                .map(|worker| {
                    s.spawn(move || {
                        let _guard = enter_worker(ids, worker);
                        let id = identity();
                        assert_eq!(id.worker_id, Some(worker));
                        id.run_id
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert_eq!(seen, ids.all().into_iter().collect());
        // the run of the batch itself (prepare/conclude) is instance 0's id
        assert_eq!(identity().run_id, ids.first);
        // a batch of another size does not reuse the scope
        assert_ne!(batch_ids(2).first, ids.first);
    }
}
