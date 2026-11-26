//! `--estimator`, `--confidence-level`, `--significance-level`,
//! `--bootstrap-resamples` and `--seed` (#320).

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn result(name: &str, times: &[f64]) -> Value {
    let n = times.len() as f64;
    let mean = times.iter().sum::<f64>() / n;
    let stddev = (times.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = (sorted[times.len() / 2] + sorted[(times.len() - 1) / 2]) / 2.0;
    json!({
        "command": name, "mean": mean, "stddev": stddev, "median": median,
        "user": 0.0, "system": 0.0, "min": sorted[0], "max": sorted[times.len() - 1],
        "times": times, "exit_codes": vec![0; times.len()],
    })
}

/// `heavy`: ten runs of 1 s and one of 100 s (mean 10.9 s but median 1 s).
/// `steady`: ten runs of about 5 s. The mean ranks `steady` first, the median `heavy`.
fn heavy_tail_fixture(dir: &Path) -> PathBuf {
    let mut heavy = vec![1.0; 9];
    heavy.push(100.0);
    let steady = [5.0, 5.0, 5.0, 5.1, 4.9, 5.0, 5.05, 4.95, 5.02, 4.98];
    let path = dir.join("fixture.json");
    std::fs::write(
        &path,
        json!({"results": [result("heavy", &heavy), result("steady", &steady)]}).to_string(),
    )
    .unwrap();
    path
}

fn export(args: &[&str], fixture: &Path) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    perfratio()
        .arg("--import-json")
        .arg(fixture)
        .args(args)
        .arg("--export-json")
        .arg(&out)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

#[test]
fn the_mean_and_the_median_rank_a_heavy_tail_differently() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = heavy_tail_fixture(dir.path());
    perfratio()
        .args(["--style=basic", "--import-json"])
        .arg(&fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("steady ran"));
    perfratio()
        .args(["--style=basic", "--estimator", "median", "--import-json"])
        .arg(&fixture)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("heavy ran")
                .and(predicate::str::contains("times faster than steady")),
        );
}

#[test]
fn a_non_mean_estimator_reports_a_ratio_interval_in_the_json() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = heavy_tail_fixture(dir.path());
    let doc = export(&["--estimator=median", "--seed=3"], &fixture);
    assert_eq!(doc["perfratio"]["statistics"]["estimator"], "median");
    let steady = &doc["results"][1];
    let ratio = steady["relative_speed"].as_f64().unwrap();
    assert!((ratio - 5.0).abs() < 0.05, "{ratio}");
    let ci = steady["relative_speed_ci"].as_array().unwrap();
    let (lo, hi) = (ci[0].as_f64().unwrap(), ci[1].as_f64().unwrap());
    assert!(lo <= ratio && ratio <= hi, "{ratio} not in [{lo}, {hi}]");
    assert!(
        steady.get("relative_speed_stddev").is_none(),
        "no error propagation for the median"
    );
    // the baseline has no interval, and the mean has no `relative_speed_ci`
    assert!(doc["results"][0].get("relative_speed_ci").is_none());
    let mean_doc = export(&[], &fixture);
    assert!(mean_doc["results"][0].get("relative_speed_ci").is_none());
    assert!(mean_doc["results"][1].get("relative_speed_ci").is_none());
}

#[test]
fn the_significance_level_decides_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = heavy_tail_fixture(dir.path());
    // the p-value of the median test is 0.094: noise at 0.05, significant at 0.2
    let at = |alpha: &str| {
        export(
            &["--estimator=median", "--significance-level", alpha],
            &fixture,
        )["results"][1]["significance"]
            .clone()
    };
    assert_eq!(at("0.2"), "significant_regression");
    assert_eq!(at("0.05"), "noise");
}

#[test]
fn default_statistics_are_exported_and_explicit_defaults_change_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = heavy_tail_fixture(dir.path());
    let default = export(&[], &fixture);
    let stats = &default["perfratio"]["statistics"];
    assert_eq!(stats["estimator"], "mean");
    assert_eq!(stats["confidence_level"], 0.95);
    assert_eq!(stats["significance_level"], 0.05);
    assert_eq!(stats["bootstrap_resamples"], 5000);
    let explicit = export(
        &[
            "--estimator=mean",
            "--confidence-level=0.95",
            "--significance-level=0.05",
            "--bootstrap-resamples=5000",
        ],
        &fixture,
    );
    assert_eq!(default["results"], explicit["results"]);
    assert_eq!(
        default["perfratio"]["statistics"],
        explicit["perfratio"]["statistics"]
    );
}

#[test]
fn the_seed_and_resamples_are_exported() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = heavy_tail_fixture(dir.path());
    let doc = export(
        &[
            "--seed=42",
            "--bootstrap-resamples=300",
            "--confidence-level=0.9",
        ],
        &fixture,
    );
    let stats = &doc["perfratio"]["statistics"];
    assert_eq!(stats["bootstrap_seed"], 42);
    assert_eq!(stats["bootstrap_resamples"], 300);
    assert_eq!(stats["confidence_level"], 0.9);
}

#[test]
fn deep_stats_follow_the_confidence_level_and_the_seed() {
    let run = |level: &str, seed: &str| {
        let out = perfratio()
            .args([
                "--debug-mode",
                "--runs=6",
                "--style=basic",
                "--deep-stats",
                "--confidence-level",
                level,
                "--seed",
                seed,
                "sleep 1",
            ])
            .assert()
            .success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    };
    assert!(run("0.9", "1").contains("Bootstrap 90% CI:"));
    assert!(run("0.999", "1").contains("Bootstrap 99.9% CI:"));
    assert!(run("0.95", "1").contains("Bootstrap 95% CI:"));
    assert_eq!(run("0.9", "5"), run("0.9", "5"));
}

#[test]
fn out_of_range_values_fail_early_and_name_the_option() {
    for (args, message) in [
        (vec!["--confidence-level=1.5"], "--confidence-level"),
        (vec!["--confidence-level=0"], "--confidence-level"),
        (vec!["--significance-level=-1"], "--significance-level"),
        (vec!["--significance-level=abc"], "--significance-level"),
        (vec!["--bootstrap-resamples=5"], "--bootstrap-resamples"),
        (vec!["--estimator=mode"], "invalid estimator"),
        (vec!["--estimator=p100"], "invalid estimator"),
    ] {
        perfratio()
            .arg("--debug-mode")
            .args(&args)
            .arg("sleep 1")
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));
    }
}

#[test]
fn compare_uses_the_estimator() {
    let dir = tempfile::tempdir().unwrap();
    // baseline: a median of 1 s, a mean of 5.9 s because of one 50 s run
    let mut base = vec![1.0, 1.01, 0.99, 1.0, 1.02, 0.98, 1.0, 1.01, 0.99];
    base.push(50.0);
    // current: steadily 2 s (a mean of 2 s)
    let cur = [2.0, 2.01, 1.99, 2.0, 2.02, 1.98, 2.0, 2.01, 1.99, 2.0];
    let write = |name: &str, times: &[f64]| {
        let path = dir.path().join(name);
        std::fs::write(
            &path,
            json!({"results": [result("app", times)]}).to_string(),
        )
        .unwrap();
        path
    };
    let (baseline, current) = (write("base.json", &base), write("cur.json", &cur));
    let compare = |estimator: &str| {
        perfratio()
            .args(["--estimator", estimator, "--import-json"])
            .arg(&current)
            .arg("--compare")
            .arg(&baseline)
            .arg("--fail-if-regressed=5%")
            .assert()
    };
    // by the mean the current result is much faster than the baseline;
    // by the median it is twice as slow, and significantly so
    compare("mean").success();
    compare("median").failure().code(3);
}
