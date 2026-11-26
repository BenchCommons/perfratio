//! Tests for --discard-outliers.

#![cfg(unix)]

mod common;
use common::perfratio;

use predicates::prelude::*;

fn run_json<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("out.json");
    perfratio()
        .args(args)
        .arg("--export-json")
        .arg(&export)
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&export).unwrap()).unwrap();
    json["results"][0].clone()
}

/// Arguments that make `--debug-mode` report 20 simulated run times: 0.01 s,
/// except 0.5 s in the given (0-based) runs. One run per value of `-L`, pooled
/// in order, so the times are exact and independent of machine load.
fn simulated(slow: &[usize]) -> Vec<String> {
    let values: Vec<&str> = (0..20)
        .map(|i| if slow.contains(&i) { "0.5" } else { "0.01" })
        .collect();
    vec![
        "--debug-mode".into(),
        "--runs=1".into(),
        "--aggregate-parameter-runs".into(),
        "-L".into(),
        "t".into(),
        values.join(","),
        "sleep {t}".into(),
    ]
}

#[test]
fn discards_a_single_outlier() {
    let mut args = simulated(&[7]);
    args.push("--discard-outliers=100".into());
    let result = run_json(&args);

    assert_eq!(result["discarded_outliers"], serde_json::json!([7]));
    assert_eq!(result["times"].as_array().unwrap().len(), 19);
    assert_eq!(result["exit_codes"].as_array().unwrap().len(), 19);
    assert!(result["max"].as_f64().unwrap() < 0.2, "{result}");
}

#[test]
fn prints_the_number_of_discarded_outliers() {
    perfratio()
        .args(simulated(&[7]))
        .args(["--style=basic", "--discard-outliers=100"])
        .assert()
        .success()
        .stdout(predicate::str::contains("19 runs (1 outlier discarded)"));
}

#[test]
fn refuses_to_discard_more_than_five_percent() {
    let dir = tempfile::tempdir().unwrap();
    let export = dir.path().join("out.json");
    perfratio()
        .args(simulated(&[3, 7, 11]))
        .arg("--discard-outliers")
        .arg("--export-json")
        .arg(&export)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "look like outliers, so none were discarded",
        ));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&export).unwrap()).unwrap();
    assert_eq!(json["results"][0]["times"].as_array().unwrap().len(), 20);
    assert!(json["results"][0].get("discarded_outliers").is_none());
}

#[test]
fn reports_original_run_numbers_together_with_omitted_failed_runs() {
    if common::is_rosetta() {
        return;
    }
    // Run 2 fails and is omitted first; run 7 is slow. The discarded run must be
    // reported as 7 (its original number), not 6 (its position after omission).
    //
    // The ordinary runs cycle through 50, 80 and 110 ms. The spread keeps the
    // median absolute deviation large, so scheduler jitter on a loaded machine
    // can't make an ordinary run look like an outlier.
    let command = "sh -c 'case $PERFRATIO_ITERATION in 2) exit 1;; 7) sleep 2;; esac; \
                   case $((PERFRATIO_ITERATION % 3)) in 0) sleep 0.05;; 1) sleep 0.08;; *) sleep 0.11;; esac'";
    let result = run_json(&[
        "-N",
        "--runs=20",
        "--ignore-failure",
        "--omit-failed-runs",
        "--discard-outliers=20",
        command,
    ]);

    assert_eq!(result["omitted_failed_runs"][0]["index"], 2);
    assert_eq!(
        result["discarded_outliers"],
        serde_json::json!([7]),
        "{result}"
    );
    assert_eq!(result["times"].as_array().unwrap().len(), 18);
}

#[test]
fn rejects_invalid_thresholds() {
    for value in ["-3", "0", "abc"] {
        perfratio()
            .arg(format!("--discard-outliers={value}"))
            .arg("true")
            .assert()
            .failure()
            .stderr(predicate::str::contains("Invalid threshold"));
    }
}
