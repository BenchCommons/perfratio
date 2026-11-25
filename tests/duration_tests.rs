//! Tests for `--duration`. `--debug-mode` simulates `sleep <s>` runs, so the
//! summed run times (and thus the run counts) are deterministic.

mod common;
use common::hyperfine;

use predicates::prelude::*;

fn export(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.json");
    hyperfine()
        .args(["--debug-mode", "--style=basic", "--export-json"])
        .arg(&file)
        .args(args)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap()
}

#[test]
fn samples_until_the_budget_is_reached_and_records_it() {
    let json = export(&["--duration", "1s", "sleep 0.1"]);
    let r = &json["results"][0];
    assert_eq!(r["runs_done"], 10);
    assert_eq!(r["times"].as_array().unwrap().len(), 10);
    assert_eq!(r["target_duration_seconds"], 1.0);
    assert!((r["total_sampling_seconds"].as_f64().unwrap() - 1.0).abs() < 1e-6);
}

#[test]
fn slow_commands_still_get_three_runs() {
    let json = export(&["--duration", "100ms", "sleep 1"]);
    assert_eq!(json["results"][0]["runs_done"], 3);
}

#[test]
fn min_and_max_runs_override_the_floor_and_cap() {
    assert_eq!(
        export(&["--duration", "100ms", "-m", "5", "sleep 1"])["results"][0]["runs_done"],
        5
    );
    assert_eq!(
        export(&["--duration", "10s", "-M", "4", "sleep 0.1"])["results"][0]["runs_done"],
        4
    );
    assert_eq!(
        export(&["--duration", "10s", "-M", "2", "sleep 0.1"])["results"][0]["runs_done"],
        2
    );
}

#[test]
fn works_with_auto_warmup_and_round_robin() {
    let json = export(&[
        "--duration",
        "1s",
        "--warmup=auto",
        "--schedule=round-robin",
        "sleep 0.1",
        "sleep 0.25",
    ]);
    assert_eq!(json["results"][0]["runs_done"], 10);
    assert_eq!(json["results"][1]["runs_done"], 4);
    assert_eq!(json["results"][0]["warmup_runs"], 5);
}

#[test]
fn fields_are_absent_without_duration() {
    let json = export(&["--runs=3", "sleep 0.1"]);
    let r = &json["results"][0];
    assert!(r.get("target_duration_seconds").is_none());
    assert!(r.get("total_sampling_seconds").is_none());
    assert!(r.get("runs_done").is_none());
}

#[test]
fn conflicting_and_invalid_options_are_rejected() {
    for args in [
        &["--duration=1s", "--runs=3"][..],
        &["--duration=1s", "--target-precision=1%"][..],
        &["--duration=0s"][..],
        &["--duration=abc"][..],
    ] {
        hyperfine()
            .arg("--debug-mode")
            .args(args)
            .arg("sleep 0.1")
            .assert()
            .failure()
            .stderr(
                predicate::str::contains("duration").or(predicate::str::contains("cannot be used")),
            );
    }
}

#[test]
fn timeout_is_compatible() {
    hyperfine()
        .args(["--debug-mode", "--duration=1s", "--timeout=2s", "sleep 0.5"])
        .assert()
        .success();
}
