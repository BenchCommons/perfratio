//! Tests for --outlier-method and the `outliers` export field.

#![cfg(unix)]

mod common;
use common::perfratio;

use predicates::prelude::*;

/// `--debug-mode` arguments simulating 20 runs of 0.01 s, except 0.3 s in the
/// given (0-based) runs. One run per `-L` value, pooled in order, so the times
/// are exact and independent of machine load.
fn simulated(slow: &[usize]) -> Vec<String> {
    let values: Vec<&str> = (0..20)
        .map(|i| if slow.contains(&i) { "0.3" } else { "0.01" })
        .collect();
    vec![
        "--debug-mode".into(),
        "--runs=1".into(),
        "--aggregate-parameter-runs".into(),
        "-L".into(),
        "t".into(),
        values.join(","),
        "sleep {t} 0.001 0.001 1000".into(),
    ]
}

fn run_json<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> (serde_json::Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("out.json");
    let out = perfratio()
        .args(args)
        .arg("--export-json")
        .arg(&export)
        .output()
        .unwrap();
    assert!(out.status.success());
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&export).unwrap()).unwrap();
    (
        json["results"][0].clone(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn reports_count_and_percentage() {
    // Exact simulated times: runs 3 and 11 are the only outliers.
    let mut args = simulated(&[3, 11]);
    args.push("--outlier-method=iqr".into());
    let (result, stderr) = run_json(&args);
    let o = &result["outliers"];
    assert_eq!(o["method"], "iqr");
    assert_eq!(o["count"], 2, "{o}");
    assert_eq!(o["percent"].as_f64().unwrap(), 10.0);
    assert!(o["lower_fence"].as_f64().unwrap() <= o["upper_fence"].as_f64().unwrap());
    assert!(o["memory_usage_byte"]["count"].is_u64());
    assert!(stderr.contains("2 of 20 runs (10.0%)"), "{stderr}");
}

#[test]
fn no_warning_and_zero_outliers_for_a_clean_sample() {
    // --debug-mode reports exactly the simulated time for every run.
    let (result, stderr) = run_json(&["--debug-mode", "--runs=20", "sleep 0.01"]);
    assert_eq!(result["outliers"]["count"], 0);
    assert_eq!(result["outliers"]["percent"], 0.0);
    assert!(!stderr.contains("of 20 runs ("), "{stderr}");
}

#[test]
fn default_method_is_modified_z() {
    let (result, _) = run_json(&["--debug-mode", "--runs=20", "sleep 0.01"]);
    assert_eq!(result["outliers"]["method"], "modified-z");
}

#[test]
fn iqr_method_is_opt_in() {
    let (result, _) = run_json(&[
        "--debug-mode",
        "--runs=20",
        "--outlier-method=iqr",
        "sleep 0.01",
    ]);
    assert_eq!(result["outliers"]["method"], "iqr");
}

#[test]
fn fewer_than_four_runs_have_no_outlier_statistics() {
    let (result, _) = run_json(&["--debug-mode", "--runs=3", "sleep 0.01"]);
    assert!(result.get("outliers").is_none());
}

#[test]
fn modified_z_method_is_selectable() {
    let (result, _) = run_json(&[
        "--debug-mode",
        "--runs=20",
        "--outlier-method=modified-z",
        "sleep 0.01",
    ]);
    assert_eq!(result["outliers"]["method"], "modified-z");
}

#[test]
fn suppress_flag_hides_the_counts() {
    perfratio()
        .args(simulated(&[3, 11]))
        .arg("--suppress-outlier-warnings")
        .assert()
        .success()
        .stderr(predicate::str::contains("Statistical outliers").not());
}

#[test]
fn rejects_unknown_method() {
    perfratio()
        .args(["--outlier-method=foo", "true"])
        .assert()
        .failure();
}
