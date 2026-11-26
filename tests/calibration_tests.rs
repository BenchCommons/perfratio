//! Tests for the runner floor, `--calibrate` and `--below-floor` (#313,
//! `docs/specs/measurement-floor.md`). The classification is asserted, never
//! exact times: they depend on the machine.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn floor_check(result: &serde_json::Value) -> &serde_json::Value {
    result["quality"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "measurement_floor")
        .expect("measurement_floor check")
}

#[test]
fn debug_mode_has_no_floor() {
    let json = export(&["--debug-mode", "--runs=3", "sleep 0.1"]);
    assert!(json["perfratio"].get("calibration").is_none());
    let result = &json["results"][0];
    assert_eq!(floor_check(result)["status"], "not_applicable");
    assert!(result["quality"].get("below_resolution").is_none());
}

#[cfg(unix)]
#[test]
fn a_trivial_command_is_below_resolution_and_a_sleep_is_not() {
    let json = export(&["-N", "--runs=20", "true", "sleep 0.1"]);
    let calibration = &json["perfratio"]["calibration"];
    assert!(
        calibration["wall_floor"].as_f64().unwrap() > 0.0,
        "{calibration}"
    );
    assert!(calibration["wall_floor_mad"].as_f64().unwrap() >= 0.0);
    assert!(calibration["runs"].as_u64().unwrap() >= 5);

    let results = json["results"].as_array().unwrap();
    assert_eq!(results[0]["quality"]["below_resolution"], true);
    assert_eq!(floor_check(&results[0])["status"], "warn");
    assert_eq!(results[1]["quality"]["below_resolution"], false);
    assert_eq!(floor_check(&results[1])["status"], "pass");
}

#[cfg(unix)]
#[test]
fn the_relative_speed_of_a_below_floor_command_is_still_shown_and_warned_about() {
    perfratio()
        .args(["-N", "--runs=20", "true", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("times faster than"))
        .stdout(predicate::str::contains("below the measurement floor"));
}

#[cfg(unix)]
#[test]
fn calibrate_off_measures_no_floor() {
    let json = export(&["-N", "--runs=5", "--calibrate=off", "true"]);
    assert!(json["perfratio"].get("calibration").is_none());
    assert_eq!(floor_check(&json["results"][0])["status"], "not_applicable");
}

#[cfg(unix)]
#[test]
fn below_floor_fail_exits_6_after_printing_the_statistics() {
    perfratio()
        .args(["-N", "--runs=20", "--below-floor=fail", "true"])
        .assert()
        .code(6)
        .stdout(predicate::str::contains("Time (mean"))
        .stderr(predicate::str::contains("below the measurement floor"));
    // a command above the floor passes
    perfratio()
        .args(["-N", "--runs=3", "--below-floor=fail", "sleep 0.5"])
        .assert()
        .success();
}

#[test]
fn invalid_values_are_rejected() {
    perfratio()
        .args(["--debug-mode", "--calibrate=sometimes", "sleep 0.1"])
        .assert()
        .failure();
    perfratio()
        .args(["--debug-mode", "--below-floor=ignore", "sleep 0.1"])
        .assert()
        .failure();
}
