//! Tests for --outlier-method and the `outliers` export field.

#![cfg(unix)]

mod common;
use common::perfratio;

use predicates::prelude::*;

fn slow_in(iterations: &str) -> String {
    format!("sh -c 'case $JOULEX_ITERATION in {iterations}) sleep 0.3;; esac; true'")
}

fn run_json(args: &[&str]) -> (serde_json::Value, String) {
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
    if common::is_rosetta() {
        return;
    }
    // Real timings are not deterministic: only check the shape and that the
    // numbers are consistent with each other and with the warning.
    let (result, stderr) = run_json(&["-N", "--runs=20", "--outlier-method=iqr", &slow_in("3|11")]);
    let o = &result["outliers"];
    assert_eq!(o["method"], "iqr");
    let count = o["count"].as_u64().expect("count is an integer");
    assert!(count <= 20, "{o}");
    assert_eq!(o["percent"].as_f64().unwrap(), count as f64 / 20.0 * 100.0);
    assert!(o["lower_fence"].as_f64().unwrap() <= o["upper_fence"].as_f64().unwrap());
    assert!(o["memory_usage_byte"]["count"].is_u64());
    if count > 0 {
        let pct = format!("{count} of 20 runs ({:.1}%)", count as f64 / 20.0 * 100.0);
        assert!(stderr.contains(&pct), "{stderr}");
    }
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
    if common::is_rosetta() {
        return;
    }
    let (result, _) = run_json(&["-N", "--runs=3", "true"]);
    assert!(result.get("outliers").is_none());
}

#[test]
fn modified_z_method_is_selectable() {
    if common::is_rosetta() {
        return;
    }
    let (result, _) = run_json(&["-N", "--runs=20", "--outlier-method=modified-z", "true"]);
    assert_eq!(result["outliers"]["method"], "modified-z");
}

#[test]
fn suppress_flag_hides_the_counts() {
    if common::is_rosetta() {
        return;
    }
    perfratio()
        .args([
            "-N",
            "--runs=20",
            "--suppress-outlier-warnings",
            &slow_in("3|11"),
        ])
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
