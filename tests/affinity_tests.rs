//! Tests for --affinity.

mod common;
use common::perfratio;

use predicates::prelude::*;

#[cfg(target_os = "linux")]
#[test]
fn benchmarked_process_runs_only_on_the_given_cpu() {
    perfratio()
        .args(["--runs=1", "-N", "--affinity=0", "--show-output"])
        .arg("grep Cpus_allowed_list /proc/self/status")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Cpus_allowed_list:\s+0\n").unwrap());
}

#[cfg(target_os = "linux")]
#[test]
fn affinity_also_applies_through_the_shell() {
    perfratio()
        .args(["--runs=1", "--affinity=0", "--show-output"])
        .arg("grep Cpus_allowed_list /proc/self/status")
        .assert()
        .success()
        .stdout(predicate::str::is_match(r"Cpus_allowed_list:\s+0\n").unwrap());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn rejects_cpus_that_do_not_exist() {
    perfratio()
        .args(["--runs=1", "--affinity=100000", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("CPU 100000 does not exist"));
}

#[cfg(target_os = "macos")]
#[test]
fn cpu_lists_are_rejected_on_macos_with_the_alternative() {
    perfratio()
        .args(["--runs=1", "--affinity=0", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not supported on macOS"))
        .stderr(predicate::str::contains("--affinity p-cores"));
}

/// `PRIO_DARWIN_BG` (e-cores) drops the scheduling priority shown by `ps` to
/// 4 (MAXPRI_THROTTLE); the default and p-cores keep the usual 31.
#[cfg(target_os = "macos")]
#[test]
fn core_classes_set_the_darwin_background_state() {
    let hybrid = std::process::Command::new("sysctl")
        .args(["-n", "hw.nperflevels"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "2")
        .unwrap_or(false);
    for (class, background) in [("e-cores", true), ("p-cores", false)] {
        let assert = perfratio()
            .args(["--runs=1", "--show-output"])
            .arg(format!("--affinity={class}"))
            .arg("echo pri=$(ps -o pri= -p $$ | tr -d ' ')")
            .assert();
        if !hybrid {
            assert
                .failure()
                .stderr(predicate::str::contains("single core type"));
            continue;
        }
        let stdout = String::from_utf8(assert.success().get_output().stdout.clone()).unwrap();
        assert_eq!(stdout.contains("pri=4\n"), background, "{class}: {stdout}");
    }
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
#[test]
fn is_rejected_where_unsupported() {
    perfratio()
        .args(["--runs=1", "--affinity=0", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only supported on Linux, Windows"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn core_classes_are_macos_only() {
    perfratio()
        .args(["--runs=1", "--affinity=e-cores", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only supported on macOS"));
}

#[test]
fn rejects_malformed_cpu_lists() {
    perfratio()
        .args(["--runs=1", "--affinity=3-1", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid '--affinity'"));
}
