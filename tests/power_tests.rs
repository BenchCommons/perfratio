//! Tests for --power / --target-effect (#367).

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::Value;

fn result(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--style=basic", "--export-json"])
        .arg(&json)
        .args(args)
        .assert()
        .success();
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    json["results"][0].clone()
}

#[test]
fn the_json_export_records_the_power_analysis() {
    let r = result(&["--debug-mode", "--runs=5", "--power=0.9", "sleep 0.1"]);
    let power = &r["power"];
    assert_eq!(power["power"], 0.9);
    assert_eq!(power["significance_level"], 0.05);
    assert_eq!(power["runs"], 5);
    assert_eq!(power["stopping_rule"], false);
    // constant mock times: any change is detectable
    assert_eq!(power["minimum_detectable_effect"], 0.0);
    assert!(power.get("target_effect").is_none());

    // Only with --power or --target-effect
    assert!(result(&["--debug-mode", "--runs=3", "sleep 0.1"])
        .get("power")
        .is_none());
}

#[test]
fn target_effect_with_runs_only_reports() {
    let r = result(&[
        "--debug-mode",
        "--runs=7",
        "--target-effect=3%",
        "sleep 0.1",
    ]);
    assert_eq!(r["times"].as_array().unwrap().len(), 7);
    let power = &r["power"];
    assert_eq!(power["power"], 0.8);
    assert_eq!(power["target_effect"], 0.03);
    assert_eq!(power["runs_needed"], 2);
    assert_eq!(power["met"], true);
    assert_eq!(power["stopping_rule"], false);
}

#[test]
fn target_effect_is_a_stopping_rule() {
    // constant times: 2 runs are enough, so the minimum number of runs decides
    let r = result(&["--debug-mode", "--target-effect=3%", "sleep 0.1"]);
    assert_eq!(r["times"].as_array().unwrap().len(), 10);
    assert_eq!(r["power"]["stopping_rule"], true);
    let r = result(&[
        "--debug-mode",
        "--target-effect=3%",
        "--min-runs=4",
        "--max-benchmarking-time=5",
        "sleep 0.1",
    ]);
    assert_eq!(r["times"].as_array().unwrap().len(), 4);
}

#[test]
fn the_report_line_is_printed() {
    perfratio()
        .args([
            "--style=basic",
            "--debug-mode",
            "--runs=5",
            "--target-effect=3%",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Power:"))
        .stdout(predicate::str::contains("±3.0% needs 2 runs"));
}

#[test]
fn invalid_values_fail_before_running() {
    for args in [
        ["--power=1.5"],
        ["--power=0"],
        ["--power=abc"],
        ["--target-effect=0"],
        ["--target-effect=-3%"],
        ["--target-effect=x"],
    ] {
        perfratio()
            .args(["--debug-mode", "--runs=3"])
            .args(args)
            .arg("sleep 0.1")
            .assert()
            .failure();
    }
}
