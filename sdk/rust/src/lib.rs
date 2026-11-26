//! Client of the perfratio region timing protocol (protocol 1, SDK 1.0.0).
//!
//! Bracket the code to measure and run the program with
//! `perfratio --region=sort ./program`:
//!
//! ```no_run
//! {
//!     let _region = perfratio_region::region("sort"); // stops when dropped, also on panic
//!     // ... the work ...
//! }
//! // or: perfratio_region::start("sort"); ...; perfratio_region::stop("sort");
//! ```
//!
//! Without perfratio (`PERFRATIO_REGION_FILE` unset or empty) every call returns at once.
//! Events are buffered in memory and written when the process exits (an `atexit`
//! handler, so `std::process::exit` and a panic that unwinds to `main` are covered) or on
//! [`flush`]. On Windows `std::process::exit`
//! skips `atexit` handlers, so call
//! [`flush`] before it. With `panic = "abort"`, a signal or `libc::_exit` the buffer is lost:
//! call [`flush`] first when that matters.
//!
//! Thread-safe; protocol 1 allows one thread per region name. Names are 1-64 ASCII letters,
//! digits or `_`; invalid names are ignored with one warning on stderr. Wire format:
//! `docs/specs/region-protocol.md` in the perfratio repository.
//!
//! Single file, no dependencies, `MIT OR Apache-2.0`: copy `src/lib.rs` into your project
//! as a module, or depend on the crate.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::Instant;

/// Protocol version this client speaks.
pub const PROTOCOL: u32 = 1;
/// Version of this client.
pub const SDK_VERSION: &str = "1.0.0";
/// Identifier sent in the `hello` line.
pub const SDK_ID: &str = "rust/1.0.0";
/// Region measured when no name is given.
pub const DEFAULT_REGION: &str = "default";

fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Buffers events and appends them to a file. [`start`], [`stop`] and [`flush`] use one
/// global recorder; a `Recorder` of your own is mainly useful for tests.
pub struct Recorder {
    path: Option<PathBuf>,
    epoch: Instant,
    events: String,
    hello_written: bool,
    warned: bool,
}

impl Recorder {
    /// `None` disables the recorder: every call does nothing.
    pub fn new(path: Option<PathBuf>) -> Self {
        Recorder {
            path: path.filter(|p| !p.as_os_str().is_empty()),
            epoch: Instant::now(),
            events: String::new(),
            hello_written: false,
            warned: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.path.is_some()
    }

    /// Open the region `name`.
    pub fn start(&mut self, name: &str) {
        self.record("start", name);
    }

    /// Close the region `name`.
    pub fn stop(&mut self, name: &str) {
        self.record("stop", name);
    }

    fn record(&mut self, kind: &str, name: &str) {
        if self.path.is_none() {
            return;
        }
        // Instant is the monotonic clock of the platform (CLOCK_MONOTONIC,
        // mach_absolute_time, QueryPerformanceCounter). A stop reads it first and a
        // start last, so the bookkeeping stays outside the region.
        let stop_time = (kind == "stop").then(|| self.epoch.elapsed());
        if !valid_name(name) {
            if !self.warned {
                self.warned = true;
                eprintln!(
                    "perfratio_region: ignoring invalid region name {name:?} \
                     (use 1-64 letters, digits or '_')"
                );
            }
            return;
        }
        let t = stop_time.unwrap_or_else(|| self.epoch.elapsed());
        let _ = writeln!(self.events, "{kind} {name} {}", t.as_nanos());
    }

    /// Append the buffered events (and the `hello` line, once) to the file.
    pub fn flush(&mut self) {
        let Some(path) = &self.path else { return };
        if self.hello_written && self.events.is_empty() {
            return;
        }
        let mut text = String::new();
        if !self.hello_written {
            let _ = writeln!(text, "hello {PROTOCOL} {SDK_ID}");
        }
        text.push_str(&self.events);
        let written = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .and_then(|mut file| file.write_all(text.as_bytes()));
        if written.is_ok() {
            // never break the program when the file cannot be written
            self.hello_written = true;
            self.events.clear();
        }
    }

    /// The events not written yet, one line each (for tests).
    pub fn pending(&self) -> &str {
        &self.events
    }
}

static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);
static REGISTER: Once = Once::new();
static ENABLED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn atexit(callback: extern "C" fn()) -> i32;
}

extern "C" fn flush_at_exit() {
    flush();
}

fn recorder() -> MutexGuard<'static, Option<Recorder>> {
    let mut guard = RECORDER.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let path = std::env::var_os("PERFRATIO_REGION_FILE").map(PathBuf::from);
        let recorder = Recorder::new(path);
        if recorder.enabled() {
            ENABLED.store(true, Ordering::Relaxed);
            REGISTER.call_once(|| {
                // SAFETY: `flush_at_exit` is a plain `extern "C"` function without arguments
                // that stays valid for the whole process.
                unsafe { atexit(flush_at_exit) };
            });
        }
        *guard = Some(recorder);
    }
    guard
}

/// Whether perfratio asked for region events.
pub fn enabled() -> bool {
    drop(recorder());
    ENABLED.load(Ordering::Relaxed)
}

/// Open the region `name`.
pub fn start(name: &str) {
    if let Some(r) = recorder().as_mut() {
        r.start(name);
    }
}

/// Close the region `name`.
pub fn stop(name: &str) {
    if let Some(r) = recorder().as_mut() {
        r.stop(name);
    }
}

/// Write the buffered events now. Done automatically at exit.
pub fn flush() {
    if let Some(r) = recorder().as_mut() {
        r.flush();
    }
}

/// Guard returned by [`region`]: stops the region when dropped, also while a panic unwinds.
#[must_use = "the region stops as soon as the guard is dropped; bind it to a variable"]
pub struct Region {
    name: String,
}

/// Start the region `name` and return a guard that stops it.
pub fn region(name: &str) -> Region {
    start(name);
    Region {
        name: name.to_string(),
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        stop(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> (tempdir::Dir, PathBuf) {
        let dir = tempdir::Dir::new();
        let path = dir.0.join("events.log");
        (dir, path)
    }

    /// Minimal temporary directory, so the crate needs no dev-dependency.
    mod tempdir {
        use std::path::PathBuf;
        pub struct Dir(pub PathBuf);
        impl Dir {
            pub fn new() -> Self {
                static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir()
                    .join(format!("perfratio-region-test-{}-{n}", std::process::id()));
                std::fs::create_dir_all(&path).unwrap();
                Dir(path)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn times(text: &str) -> Vec<u128> {
        text.lines()
            .filter(|l| !l.starts_with("hello"))
            .map(|l| l.rsplit(' ').next().unwrap().parse().unwrap())
            .collect()
    }

    #[test]
    fn disabled_recorder_does_nothing() {
        for path in [None, Some(PathBuf::new())] {
            let mut r = Recorder::new(path);
            assert!(!r.enabled());
            r.start("a");
            r.stop("a");
            r.flush();
            assert_eq!(r.pending(), "");
        }
    }

    #[test]
    fn buffers_until_flush_and_writes_hello_once() {
        let (_dir, path) = file();
        let mut r = Recorder::new(Some(path.clone()));
        r.start("a");
        r.stop("a");
        assert!(!path.exists(), "nothing is written before flush");
        r.flush();
        r.start("b");
        r.stop("b");
        r.flush();
        r.flush(); // nothing new: no change
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "hello 1 rust/1.0.0");
        assert_eq!(lines.len(), 5, "{text}");
        assert_eq!(text.matches("hello").count(), 1);
        assert!(lines[1].starts_with("start a "));
        assert!(lines[2].starts_with("stop a "));
        assert!(lines[3].starts_with("start b "));
    }

    #[test]
    fn hello_alone_marks_an_instrumented_program() {
        let (_dir, path) = file();
        let mut r = Recorder::new(Some(path.clone()));
        r.flush();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello 1 rust/1.0.0\n");
    }

    #[test]
    fn timestamps_are_monotonic_and_measure_the_sleep() {
        let (_dir, path) = file();
        let mut r = Recorder::new(Some(path.clone()));
        r.start("a");
        std::thread::sleep(std::time::Duration::from_millis(20));
        r.stop("a");
        r.flush();
        let t = times(&std::fs::read_to_string(&path).unwrap());
        assert!(t[1] >= t[0]);
        assert!(t[1] - t[0] >= 19_000_000, "{t:?}");
    }

    #[test]
    fn invalid_names_are_dropped() {
        let (_dir, path) = file();
        let mut r = Recorder::new(Some(path.clone()));
        r.start("has space");
        r.start("");
        r.start(&"x".repeat(65));
        r.start("a-b");
        assert_eq!(r.pending(), "");
        r.start(&"x".repeat(64));
        assert_eq!(r.pending().lines().count(), 1);
    }

    #[test]
    fn unwritable_file_keeps_the_events_and_never_panics() {
        let (_dir, path) = file();
        let mut r = Recorder::new(Some(path.join("missing").join("events.log")));
        r.start("a");
        r.flush();
        assert!(r.pending().contains("start a"));
    }

    #[test]
    fn guard_stops_the_region_when_dropped() {
        // the global recorder is disabled in unit tests (no PERFRATIO_REGION_FILE)
        assert!(!enabled());
        let _guard = region("a");
        start("b");
        stop("b");
        flush();
    }
}
