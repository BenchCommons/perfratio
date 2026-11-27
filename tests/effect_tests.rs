//! `--noise-threshold` and `--equivalence-margin` (#323).

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
    json!({
        "command": name, "mean": mean, "stddev": stddev,
        "median": sorted[times.len() / 2],
        "user": 0.0, "system": 0.0, "min": sorted[0], "max": sorted[times.len() - 1],
        "times": times, "exit_codes": vec![0; times.len()],
    })
}

const REF: [f64; 8] = [10.0, 10.4, 9.8, 10.1, 10.3, 9.9, 10.2, 10.0];
const OTHER: [f64; 9] = [10.3, 10.6, 10.1, 10.5, 10.2, 10.7, 10.4, 10.35, 10.45];

/// `ref` (mean 10.19) and `other` (mean 10.5, +3.07%): the Welch TOST p-value is
/// 0.032 for a 5% margin and 0.54 for 3% (statsmodels ttost_ind).
fn fixture(dir: &Path) -> PathBuf {
    let path = dir.join("fixture.json");
    std::fs::write(
        &path,
        json!({"results": [result("ref", &REF), result("other", &OTHER)]}).to_string(),
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

fn other(doc: &Value) -> &Value {
    &doc["results"][1]
}

#[test]
fn the_noise_threshold_overrides_the_p_value() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    let class =
        |pct: &str| export(&["--noise-threshold", pct], &f)["results"][1]["classification"].clone();
    assert_eq!(class("5"), "within_noise");
    assert_eq!(class("5%"), "within_noise");
    assert_eq!(class("1"), "slower");
}

#[test]
fn the_equivalence_test_decides_between_equivalent_and_inconclusive() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    let at = |margin: &str| export(&["--equivalence-margin", margin], &f);

    let equivalent = at("5");
    assert_eq!(other(&equivalent)["classification"], "equivalent");
    // statsmodels ttost_ind, usevar='unequal'
    let p = other(&equivalent)["equivalence_p_value"].as_f64().unwrap();
    assert!((p - 0.03176655285179594).abs() < 1e-6, "{p}");

    // significantly different and not shown to be within the margin
    assert_eq!(other(&at("4"))["classification"], "slower");
    assert_eq!(other(&at("3"))["classification"], "slower");
}

/// Two noisy commands: +4.7%, p about 0.5, so neither different nor equivalent.
const NOISY_REF: [f64; 8] = [10.0, 12.0, 8.0, 11.0, 9.0, 10.5, 9.5, 10.0];
const NOISY_OTHER: [f64; 8] = [10.5, 12.5, 8.5, 11.5, 9.5, 11.0, 10.0, 10.5];

fn noisy_fixture(dir: &Path) -> PathBuf {
    let path = dir.join("noisy.json");
    std::fs::write(
        &path,
        json!({"results": [result("ref", &NOISY_REF), result("other", &NOISY_OTHER)]}).to_string(),
    )
    .unwrap();
    path
}

#[test]
fn an_unsettled_comparison_is_inconclusive_with_a_run_count() {
    let dir = tempfile::tempdir().unwrap();
    let f = noisy_fixture(dir.path());
    let doc = export(&["--equivalence-margin", "10"], &f);
    assert_eq!(other(&doc)["classification"], "inconclusive");
    let runs = other(&doc)["effect"]["runs_needed"].as_u64().unwrap();
    // (v1 + v2) (z(0.95) + z(0.9))^2 / (margin - |diff|)^2, margin 10% of 10 and diff 0.5
    let variance = |xs: &[f64]| {
        let m = xs.iter().sum::<f64>() / xs.len() as f64;
        xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0)
    };
    let z: f64 = 1.644853626951472 + 1.2815515655446004;
    let room: f64 = 0.1 * (NOISY_REF.iter().sum::<f64>() / 8.0) - 0.5;
    let expected = ((variance(&NOISY_REF) + variance(&NOISY_OTHER)) * z * z / (room * room)).ceil();
    assert_eq!(runs, expected as u64);
    assert!(runs > 8);
    // a margin below the observed change cannot be met: no run count
    let tight = export(&["--equivalence-margin", "2"], &f);
    assert!(other(&tight)["effect"].get("runs_needed").is_none());
}

#[test]
fn the_effect_has_an_interval_and_a_standardized_size() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    let doc = export(&["--noise-threshold", "1"], &f);
    let effect = &other(&doc)["effect"];
    let mean_ref = REF.iter().sum::<f64>() / REF.len() as f64;
    let rel = |v: f64| v / mean_ref;
    let close = |a: &Value, b: f64| (a.as_f64().unwrap() - b).abs() < 1e-9;
    // scipy: diff 0.3125, 95% Welch interval [0.108309, 0.516691]
    assert!(close(&effect["relative_change"], rel(0.3125)));
    assert!(close(&effect["ci_low"], rel(0.10830898126356606)));
    assert!(close(&effect["ci_high"], rel(0.516691018736434)));
    assert!(close(
        &effect["standardized"]["cohens_d"],
        1.5978437267653556
    ));
    assert!(close(&effect["standardized"]["common_language"], 0.875));
    assert!(effect["minimum_detectable_effect"].as_f64().unwrap() > 0.0);
}

#[test]
fn the_median_estimator_is_classified_with_a_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    let doc = export(&["--estimator=median", "--equivalence-margin=10"], &f);
    assert_eq!(other(&doc)["classification"], "equivalent");
    assert!(other(&doc).get("equivalence_p_value").is_none());
}

#[test]
fn the_settings_are_exported_and_the_default_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    let doc = export(&["--noise-threshold=2", "--equivalence-margin=7.5"], &f);
    let statistics = &doc["perfratio"]["statistics"];
    assert_eq!(statistics["noise_threshold"], 0.02);
    assert_eq!(statistics["equivalence_margin"], 0.075);

    let plain = export(&[], &f);
    let statistics = &plain["perfratio"]["statistics"];
    assert!(statistics.get("noise_threshold").is_none());
    assert!(statistics.get("equivalence_margin").is_none());
    let entry = other(&plain);
    for key in ["effect", "classification", "equivalence_p_value"] {
        assert!(entry.get(key).is_none(), "{key}");
    }
    assert!(entry.get("significance").is_some());
}

#[test]
fn the_summary_has_markers_for_equivalent_inconclusive_and_within_noise() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    let run = |args: &[&str]| {
        let out = perfratio()
            .arg("--import-json")
            .arg(&f)
            .args(["--style", "basic"])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    assert!(run(&["--equivalence-margin=5"])
        .contains("[=] equivalent within ±5% (+3.1%, TOST p = 0.032)"));
    let noisy = noisy_fixture(dir.path());
    let out = perfratio()
        .arg("--import-json")
        .arg(&noisy)
        .args(["--style", "basic", "--equivalence-margin=10"])
        .output()
        .unwrap();
    let inconclusive = String::from_utf8(out.stdout).unwrap();
    assert!(
        inconclusive.contains("[?] inconclusive, more runs needed")
            && inconclusive.contains("runs per command would settle it"),
        "{inconclusive}"
    );
    assert!(run(&["--noise-threshold=5"]).contains("[~] within noise (+3.1%, threshold 5%)"));
    // without the options the marker is the one of #215
    assert!(run(&[]).contains("[-] significant regression"));
}

#[test]
fn noise_threshold_is_still_accepted_with_pareto_and_validated() {
    let dir = tempfile::tempdir().unwrap();
    let f = fixture(dir.path());
    perfratio()
        .arg("--import-json")
        .arg(&f)
        .args(["--pareto=wall", "--noise-threshold=5"])
        .assert()
        .success();
    for (flag, value) in [
        ("--noise-threshold", "-1"),
        ("--noise-threshold", "abc"),
        ("--equivalence-margin", "0"),
        ("--equivalence-margin", "100"),
        ("--equivalence-margin", "x"),
    ] {
        perfratio()
            .arg("--import-json")
            .arg(&f)
            .arg(format!("{flag}={value}"))
            .assert()
            .failure()
            .stderr(predicate::str::contains(flag));
    }
}

#[cfg(unix)]
mod compare {
    use super::*;

    fn baseline(dir: &Path) -> PathBuf {
        let path = dir.join("baseline.json");
        perfratio()
            .args(["--debug-mode", "--runs=5", "-n", "app", "sleep 0.1"])
            .arg("--export-json")
            .arg(&path)
            .assert()
            .success();
        path
    }

    fn compare(base: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
        perfratio()
            .args([
                "--debug-mode",
                "--runs=5",
                "--fail-if-regressed=1%",
                "--compare",
            ])
            .arg(base)
            .args(extra)
            .args(["-n", "app", "sleep 0.103"])
            .assert()
    }

    #[test]
    fn fail_if_regressed_ignores_changes_within_noise() {
        let dir = tempfile::tempdir().unwrap();
        let base = baseline(dir.path());
        compare(&base, &[])
            .code(3)
            .stdout(predicate::str::contains("REGRESSION"));
        compare(&base, &["--noise-threshold=5"])
            .success()
            .stdout(predicate::str::contains("within noise"));
        compare(&base, &["--noise-threshold=1"]).code(3);
    }

    #[test]
    fn fail_if_regressed_ignores_equivalent_results() {
        let dir = tempfile::tempdir().unwrap();
        let base = baseline(dir.path());
        compare(&base, &["--equivalence-margin=5"])
            .success()
            .stdout(predicate::str::contains("equivalent"));
    }
}
