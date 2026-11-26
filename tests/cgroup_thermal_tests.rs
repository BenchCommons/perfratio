//! Tests for the cgroup throttling and temperature diagnostics and for
//! `--cooldown` (#315, `docs/specs/cgroup-thermal.md`). The `/proc` and `/sys`
//! files come from a fixture tree named by `PERFRATIO_SYSTEM_CHECK_ROOT`.

mod common;
use common::perfratio;

use predicates::prelude::*;
use std::path::Path;
use std::time::{Duration, Instant};

const ROOT: &str = "PERFRATIO_SYSTEM_CHECK_ROOT";

fn write(root: &Path, path: &str, content: &str) {
    let full = root.join(path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn thermal_root(millidegrees: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "sys/class/thermal/thermal_zone0/temp",
        millidegrees,
    );
    dir
}

fn export(args: &[&str], root: &Path) -> serde_json::Value {
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("out.json");
    perfratio()
        .env(ROOT, root)
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn stability(result: &serde_json::Value) -> &serde_json::Value {
    result["quality"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "environment_stability")
        .expect("environment_stability check")
}

#[test]
fn debug_mode_without_a_fixture_observes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    perfratio()
        .args(["--debug-mode", "--runs=3", "sleep 0.1", "--export-json"])
        .arg(&out)
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    assert!(json["results"][0].get("interference").is_none());
    assert_eq!(stability(&json["results"][0])["status"], "not_applicable");
}

#[test]
fn temperature_is_traced_and_a_cool_host_passes() {
    let root = thermal_root("52000\n");
    let json = export(&["--debug-mode", "--runs=3", "sleep 0.1"], root.path());
    let result = &json["results"][0];
    let thermal = &result["interference"]["thermal"];
    assert_eq!(thermal["start_c"], 52.0);
    assert_eq!(thermal["max_c"], 52.0);
    assert!(result["interference"]
        .get("cgroup_throttled_usec")
        .is_none());
    assert_eq!(stability(result)["status"], "pass");
}

#[cfg(unix)]
mod unix {
    use super::*;

    /// A command that bumps the fixture's cgroup throttle counters on every run
    fn throttling_command(root: &Path) -> String {
        let stat = root.join("sys/fs/cgroup/job/cpu.stat");
        let counter = root.join("counter");
        std::fs::write(&counter, "0").unwrap();
        format!(
            "n=$(cat '{counter}'); n=$((n+1)); echo $n > '{counter}'; \
             printf 'nr_throttled %s\\nthrottled_usec %s\\n' $n $((n*2500)) > '{stat}'",
            counter = counter.display(),
            stat = stat.display()
        )
    }

    fn cgroup_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "proc/self/cgroup", "0::/job\n");
        write(r, "sys/fs/cgroup/cgroup.controllers", "cpu\n");
        write(r, "sys/fs/cgroup/job/cpu.max", "20000 100000\n");
        write(
            r,
            "sys/fs/cgroup/job/cpu.stat",
            "nr_throttled 0\nthrottled_usec 0\n",
        );
        dir
    }

    #[test]
    fn a_throttled_cgroup_is_reported_and_warned_about() {
        let root = cgroup_root();
        let command = throttling_command(root.path());
        let json = export(&["--shell=default", "--runs=4", &command], root.path());
        let result = &json["results"][0];
        let interference = &result["interference"];
        assert_eq!(interference["cgroup_throttled_runs"], 4, "{interference}");
        assert!(interference["cgroup_throttled_usec"].as_u64().unwrap() >= 10_000);
        let check = stability(result);
        assert_eq!(check["status"], "warn");
        assert!(check["detail"]
            .as_str()
            .unwrap()
            .contains("CPU quota throttled"));
        assert_ne!(result["quality"]["grade"], "comparable");
    }

    #[test]
    fn an_idle_cgroup_reports_zero_because_the_counter_was_read() {
        let root = cgroup_root();
        let json = export(&["--shell=default", "--runs=3", "true"], root.path());
        let result = &json["results"][0];
        assert_eq!(result["interference"]["cgroup_throttled_usec"], 0);
        assert_eq!(result["interference"]["cgroup_throttled_runs"], 0);
        assert_eq!(stability(result)["status"], "pass");
    }
}

#[test]
fn cooldown_waits_between_runs_but_not_before_the_first() {
    let started = Instant::now();
    perfratio()
        .args(["--debug-mode", "--runs=4", "--cooldown=150ms", "sleep 0.1"])
        .assert()
        .success();
    // three waits of 150 ms (the mock does not sleep)
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn until_temp_gives_up_after_the_timeout_with_a_message() {
    let root = thermal_root("90000\n");
    let started = Instant::now();
    perfratio()
        .env(ROOT, root.path())
        .args([
            "--debug-mode",
            "--runs=1",
            "--cooldown=until-temp=60",
            "--cooldown-timeout=300ms",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("still at 90 °C"))
        .stderr(predicate::str::contains("--cooldown-timeout"));
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[test]
fn until_temp_continues_at_once_when_the_host_is_cool() {
    let root = thermal_root("40000\n");
    let started = Instant::now();
    perfratio()
        .env(ROOT, root.path())
        .args([
            "--debug-mode",
            "--runs=3",
            "--cooldown=until-temp=60",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("still at").not());
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn until_temp_without_a_sensor_warns_once_and_does_not_wait() {
    let root = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let assert = perfratio()
        .env(ROOT, root.path())
        .args([
            "--debug-mode",
            "--runs=3",
            "--cooldown=until-temp=60",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("no temperature sensor"));
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert_eq!(
        stderr.matches("no temperature sensor").count(),
        1,
        "{stderr}"
    );
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn invalid_cooldown_values_are_rejected() {
    for value in ["soon", "until-temp=hot", "until-temp=0"] {
        perfratio()
            .args(["--debug-mode", &format!("--cooldown={value}"), "sleep 0.1"])
            .assert()
            .failure();
    }
    perfratio()
        .args(["--debug-mode", "--cooldown-timeout=1s", "sleep 0.1"])
        .assert()
        .failure();
}
