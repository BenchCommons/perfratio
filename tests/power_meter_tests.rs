//! `--power-meter` (#382). The meter is a shell command, so a script that prints a number plays
//! the meter on any Unix system, with the real executor. No hardware is needed.
#![cfg(unix)]

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export(args: &[&str]) -> (serde_json::Value, assert_cmd::assert::Assert) {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    let assert = perfratio()
        .args(["--runs=3", "--warmup=0", "-N", "--energy"])
        .args(args)
        .arg("--export-json")
        .arg(&out)
        .arg("sleep 0.2")
        .assert();
    let json = std::fs::read_to_string(&out)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(serde_json::Value::Null);
    (json, assert)
}

#[test]
fn a_constant_power_meter_gives_power_times_time() {
    let (json, assert) = export(&[
        "--power-meter",
        "echo 20 W",
        "--power-meter-interval",
        "20ms",
    ]);
    assert.success();
    assert_eq!(json["perfratio"]["energy_backend"], "external_meter");
    assert_eq!(json["perfratio"]["measurements"]["energy"], "measured");
    let r = &json["results"][0];
    let watts = r["mean_watts"].as_f64().unwrap();
    assert!((watts - 20.0).abs() < 1.0, "{watts} W");
    assert_eq!(r["energy_joules"].as_array().unwrap().len(), 3);
    // the command line is not exported: it may hold a token in a URL
    assert!(!json["perfratio"].to_string().contains("echo 20 W"));
}

#[test]
fn units_are_converted() {
    let (json, assert) = export(&[
        "--power-meter",
        "echo 5000 mW",
        "--power-meter-interval=20ms",
    ]);
    assert.success();
    let watts = json["results"][0]["mean_watts"].as_f64().unwrap();
    assert!((watts - 5.0).abs() < 0.5, "{watts} W");
}

#[test]
fn a_counter_meter_is_read_before_and_after_each_run() {
    // The counter grows by 1 Wh (3600 J) on every reading
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("n");
    std::fs::write(&state, "0").unwrap();
    let script = format!(
        "n=$(cat '{0}'); n=$((n+1)); echo $n > '{0}'; echo \"$n Wh\"",
        state.display()
    );
    let (json, assert) = export(&["--power-meter", &script, "--power-meter-mode", "counter"]);
    assert.success();
    let energy = json["results"][0]["energy_joules"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(energy.len(), 3);
    // readings: probe, then (before, after) per run: each run sees exactly one step, scaled by
    // the share of the window that the command's own time covers
    for e in energy {
        let j = e.as_f64().unwrap();
        assert!(j > 3000.0 && j <= 3600.0, "{j} J");
    }
}

#[test]
fn a_broken_meter_degrades_with_a_clear_warning() {
    let (json, assert) = export(&["--power-meter", "echo not-a-number"]);
    assert
        .success()
        .stderr(predicate::str::contains(
            "external power meter cannot be read",
        ))
        .stderr(predicate::str::contains("not a number"));
    assert_eq!(json["perfratio"]["measurements"]["energy"], "unavailable");
    assert_eq!(
        json["perfratio"]["measurements"]["energy_unavailable_reason"],
        "meter_unreadable"
    );
    assert!(json["results"][0].get("mean_energy_joules").is_none());
}

#[test]
fn a_failing_meter_command_is_unreadable() {
    let (_, assert) = export(&["--power-meter", "exit 4"]);
    assert
        .success()
        .stderr(predicate::str::contains("the meter command failed"));
}

#[test]
fn the_meter_backend_needs_a_command() {
    perfratio()
        .args(["--runs=1", "--energy", "--energy-backend", "meter", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--power-meter"));
}

#[test]
fn the_meter_conflicts_with_a_hardware_backend() {
    perfratio()
        .args([
            "--runs=1",
            "--energy",
            "--energy-backend",
            "sysfs",
            "--power-meter",
            "echo 1",
            "true",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflicts"));
}

#[test]
fn the_meter_needs_energy() {
    perfratio()
        .args(["--runs=1", "--power-meter", "echo 1", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--energy"));
}

#[test]
fn the_meter_options_are_validated() {
    perfratio()
        .args([
            "--energy",
            "--power-meter",
            "echo 1",
            "--power-meter-mode",
            "x",
            "true",
        ])
        .assert()
        .failure();
    perfratio()
        .args([
            "--energy",
            "--power-meter",
            "echo 1",
            "--power-meter-interval",
            "soon",
            "true",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--power-meter-interval"));
    perfratio()
        .args(["--energy", "--power-meter-mode", "counter", "true"])
        .assert()
        .failure();
}

#[test]
fn the_meter_can_be_combined_with_attribution() {
    let (json, assert) = export(&[
        "--power-meter",
        "echo 10",
        "--power-meter-interval=20ms",
        "--energy-attribution",
        "cpu-time",
    ]);
    assert.success();
    assert_eq!(json["perfratio"]["energy_backend"], "external_meter");
}
