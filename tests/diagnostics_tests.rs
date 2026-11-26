//! Tests for the interference diagnostics: trend, multimodality and
//! outlier-inflated variance warnings, and the `diagnostics` JSON block.

mod common;
use common::perfratio;

use predicates::prelude::*;

#[test]
fn stable_benchmarks_have_no_diagnostic_warnings() {
    perfratio()
        .args(["--debug-mode", "--runs=40", "sleep 0.1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("trend").not())
        .stderr(predicate::str::contains("multimodal").not())
        .stderr(predicate::str::contains("variance").not());
}

/// `--debug-mode` reports exactly the simulated times, so the diagnostics
/// are independent of machine load. With one run per `-P`/`-L` value and
/// `--aggregate-parameter-runs`, the pooled times arrive in value order.
///
/// A ramp from 10 ms to 39 ms in 1 ms steps: a clear upward trend.
const RAMP: [&str; 6] = [
    "--aggregate-parameter-runs",
    "-P",
    "i",
    "10",
    "39",
    "sleep 0.0{i}",
];
/// Alternates between 50 ms and 150 ms: two separated groups.
const TWO_GROUPS: [&str; 5] = [
    "--aggregate-parameter-runs",
    "-L",
    "t",
    "05,15,05,15,05,15,05,15,05,15,05,15,05,15,05,15,05,15,05,15,\
     05,15,05,15,05,15,05,15,05,15,05,15,05,15,05,15,05,15,05,15",
    "sleep 0.{t}",
];

#[test]
fn a_ramp_triggers_the_trend_warning_and_is_exported() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--debug-mode", "--runs=1", "--export-json"])
        .arg(&json)
        .args(RAMP)
        .assert()
        .success()
        .stderr(predicate::str::contains("systematic upward trend"));

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let diagnostics = &json["results"][0]["diagnostics"];
    assert!(diagnostics["trend_rel"].as_f64().unwrap() > 0.5);
    assert!(diagnostics["trend_p"].as_f64().unwrap() < 0.001);
}

#[test]
fn two_groups_trigger_the_multimodal_warning() {
    perfratio()
        .args(["--debug-mode", "--runs=1", "--first-run=discard"])
        .args(TWO_GROUPS)
        .assert()
        .success()
        .stderr(predicate::str::contains("looks multimodal"))
        // The slower group is a mode, not "outliers"
        .stderr(predicate::str::contains("of the variance").not());
}

#[test]
fn diagnostic_warnings_can_be_suppressed() {
    perfratio()
        .args(["--debug-mode", "--runs=1", "--suppress-outlier-warnings"])
        .args(RAMP)
        .assert()
        .success()
        .stderr(predicate::str::contains("trend").not());
}
