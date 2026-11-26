//! Tests for process-tree cancellation and leftover detection (#309).
//! The commands start background children, so they are Unix-only; the
//! daemonizing test also needs Linux (`setsid`, the subreaper).
#![cfg(unix)]

mod common;
use common::perfratio_raw_command;

use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Wait until `path` holds a pid written by a test command.
fn wait_for_pid(path: &Path) -> i32 {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "no pid in {path:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn is_gone(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let ret = unsafe { libc::kill(pid, 0) };
    ret != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// A process may take a moment to disappear after SIGKILL.
fn assert_gone(pid: i32) {
    let start = Instant::now();
    while !is_gone(pid) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "process {pid} is still running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn a_command_without_children_is_not_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    perfratio_raw_command()
        .args(["-N", "--runs=5", "--style=none", "--export-json"])
        .arg(&out)
        .arg("true")
        .status()
        .unwrap();
    assert!(read_json(&out)["results"][0]
        .get("leftover_processes")
        .is_none());
}

#[test]
fn a_background_child_is_killed_and_the_run_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    let pidfile = dir.path().join("pid");
    let command = format!("sleep 31 & echo $! > '{}'; sleep 0.05", pidfile.display());
    let output = perfratio_raw_command()
        .args(["--runs=2", "--style=basic", "--export-json"])
        .arg(&out)
        .arg(&command)
        .output()
        .unwrap();
    assert!(output.status.success());

    assert_eq!(read_json(&out)["results"][0]["leftover_processes"], true);
    assert_gone(wait_for_pid(&pidfile));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        format!("{stdout}{stderr}").contains("still running when it exited"),
        "{stdout}{stderr}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_daemonized_child_is_found_through_the_subreaper() {
    if !Path::new("/usr/bin/setsid").exists() && !Path::new("/bin/setsid").exists() {
        eprintln!("skipped: no setsid");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    let pidfile = dir.path().join("pid");
    // the child leaves the process group and the session, and its parent exits
    let command = format!(
        "setsid sleep 32 </dev/null >/dev/null 2>&1 & echo $! > '{}'; sleep 0.05",
        pidfile.display()
    );
    let status = perfratio_raw_command()
        .args(["--runs=2", "--style=none", "--export-json"])
        .arg(&out)
        .arg(&command)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(read_json(&out)["results"][0]["leftover_processes"], true);
    assert_gone(wait_for_pid(&pidfile));
}

#[test]
fn timeout_kills_the_whole_tree() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let command = format!("sleep 33 & echo $! > '{}'; sleep 33", pidfile.display());
    let start = Instant::now();
    let status = perfratio_raw_command()
        .args(["--timeout=300ms", "--runs=2", "--style=none"])
        .arg(&command)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_gone(wait_for_pid(&pidfile));
}

/// Start perfratio on `command`, wait for the command to write its pids,
/// give it `delay`, send SIGINT and return the exit code and the pids.
fn interrupt(
    extra: &[&str],
    command: &str,
    pidfiles: &[&Path],
    out: &Path,
    delay: Duration,
) -> (i32, Vec<i32>) {
    let mut child = perfratio_raw_command()
        .args(extra)
        .args(["--style=basic", "--export-json"])
        .arg(out)
        .arg(command)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pids: Vec<i32> = pidfiles.iter().map(|p| wait_for_pid(p)).collect();
    std::thread::sleep(delay);
    // SAFETY: plain syscall on our own child.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let status = child.wait().unwrap();
    (status.code().unwrap_or(-1), pids)
}

#[test]
fn ctrl_c_kills_the_tree_of_the_run_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    let out = dir.path().join("out.json");
    let command = format!("sleep 34 & echo $! > '{}'; sleep 34", pidfile.display());
    let (code, pids) = interrupt(&["--runs=20"], &command, &[&pidfile], &out, Duration::ZERO);
    assert_eq!(code, 130);
    assert_gone(pids[0]);
}

#[test]
fn ctrl_c_kills_every_instance_of_a_concurrent_batch() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    let base = dir.path().join("pid");
    let pidfiles: Vec<_> = (0..3)
        .map(|n| dir.path().join(format!("pid.{n}")))
        .collect();
    let command = format!(
        "sleep 35 & echo $! > '{}.'$PERFRATIO_WORKER_ID; sleep 35",
        base.display()
    );
    let refs: Vec<&Path> = pidfiles.iter().map(|p| p.as_path()).collect();
    let (code, pids) = interrupt(
        &["--runs=20", "--concurrency=3"],
        &command,
        &refs,
        &out,
        Duration::from_millis(200),
    );
    assert_eq!(code, 130);
    for pid in pids {
        assert_gone(pid);
    }
}

#[test]
fn an_interrupted_batch_is_not_part_of_the_statistics() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    let started = dir.path().join("pid");
    let command = format!("echo $$ > '{}'; sleep 0.3", started.display());
    let (code, _) = interrupt(
        &["--runs=200", "--concurrency=2", "--warmup=0"],
        &command,
        &[&started],
        &out,
        Duration::from_millis(1500),
    );
    assert_eq!(code, 130);
    let json = read_json(&out);
    let result = &json["results"][0];
    let times = result["times"].as_array().unwrap().len();
    assert!(times < 200, "{times}");
    // only whole batches: N exit codes and one row of instance times per run
    assert_eq!(result["exit_codes"].as_array().unwrap().len(), times * 2);
    assert_eq!(result["instance_times"].as_array().unwrap().len(), times);
    assert_eq!(json["interrupted"], true);
}
