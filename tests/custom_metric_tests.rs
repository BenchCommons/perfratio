//! Tests for `--metric custom:<name>` (#90, #48).

mod common;
use common::perfratio;

use predicates::prelude::*;

fn json_of(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .args(["--export-json", path.to_str().unwrap()])
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn custom_metric_ranks_by_lower_value() {
    let json = json_of(&[
        "--runs=2",
        "--warmup=0",
        "--output-metric",
        "v=v=([0-9.]+)",
        "--metric",
        "custom:v",
        "echo v=5",
        "echo v=2",
    ]);
    assert_eq!(json["metric"], "custom:v");
    let first = &json["results"][0];
    let second = &json["results"][1];
    // the command with the lower value is the baseline
    assert_eq!(second["relative_speed"], 1.0);
    assert_eq!(first["relative_speed"], 2.5);
    assert_eq!(first["primary_metric"]["metric"], "custom:v");
    assert_eq!(first["primary_metric"]["mean"], 5.0);
    assert_eq!(
        first["primary_metric"]["samples"],
        serde_json::json!([5.0, 5.0])
    );
}

#[test]
fn custom_metric_terminal_output_names_the_metric() {
    perfratio()
        .args([
            "--runs=2",
            "--warmup=0",
            "--output-metric",
            "v=v=([0-9.]+)",
            "--metric",
            "custom:v",
            "echo v=5",
            "echo v=2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("v (mean ± σ):"))
        .stdout(predicate::str::contains("had"))
        .stdout(predicate::str::contains("2.50"))
        .stdout(predicate::str::contains("times lower value than"))
        // printed once per benchmark, not repeated as a secondary metric
        .stdout(predicate::str::contains("v (mean ± σ):").count(2));
}

#[test]
fn custom_metric_requires_matching_output_metric() {
    perfratio()
        .args(["--metric", "custom:nope", "echo v=1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'--metric custom:nope' needs a matching '--output-metric nope=<REGEX>'",
        ));

    perfratio()
        .args([
            "--output-metric",
            "v=v=([0-9]+)",
            "--metric",
            "custom:other",
            "echo v=1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("custom:other"));
}

#[test]
fn custom_metric_rejects_malformed_selector() {
    perfratio()
        .args(["--metric", "custom:", "echo 1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("expected 'custom:<name>'"));
}

#[test]
fn custom_metric_fails_when_no_run_produced_a_value() {
    perfratio()
        .args([
            "--runs=2",
            "--warmup=0",
            "-i",
            "--output-metric",
            "v=v=([0-9]+)",
            "--metric",
            "custom:v",
            "echo nothing",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no run produced 'v'"));
}

#[cfg(unix)]
#[test]
fn custom_metric_fails_when_only_some_runs_produced_a_value() {
    perfratio()
        .args([
            "--runs=3",
            "--warmup=0",
            "-i",
            "--output-metric",
            "v=v=([0-9]+)",
            "--metric",
            "custom:v",
            "if [ \"$PERFRATIO_ITERATION\" != 1 ]; then echo v=$((PERFRATIO_ITERATION + 10)); fi",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("1 of 3 runs have none for 'v'"));
}

#[test]
fn custom_metric_import_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.json");
    perfratio()
        .args([
            "--runs=2",
            "--warmup=0",
            "--output-metric",
            "v=v=([0-9.]+)",
            "--metric",
            "custom:v",
            "--export-json",
            path.to_str().unwrap(),
            "echo v=5",
            "echo v=2",
        ])
        .assert()
        .success();

    perfratio()
        .args([
            "--metric",
            "custom:v",
            "--import-json",
            path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("2.50"));

    // an import made with another metric is rejected
    perfratio()
        .args(["--metric", "cpu", "--import-json", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("custom:v"));
}
