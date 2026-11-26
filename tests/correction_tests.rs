//! `--correction` and `--comparison-family` (#321).

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

/// xorshift64*, so that the fixtures are the same on every platform
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let v = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Approximately normal (sum of 12 uniforms), mean 1 s, sd 0.1 s
    fn sample(&mut self) -> f64 {
        1.0 + ((0..12).map(|_| self.next()).sum::<f64>() - 6.0) * 0.1
    }
}

/// `count` commands that all draw their 20 runs from the same distribution.
fn identical_commands(dir: &Path, count: usize) -> PathBuf {
    let mut rng = Rng(42);
    let results: Vec<Value> = (0..count)
        .map(|i| {
            let times: Vec<f64> = (0..20).map(|_| rng.sample()).collect();
            result(&format!("cmd{i}"), &times)
        })
        .collect();
    let path = dir.join("identical.json");
    std::fs::write(&path, json!({ "results": results }).to_string()).unwrap();
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

fn significant(doc: &Value) -> usize {
    doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            matches!(
                r["significance"].as_str(),
                Some("significant_speedup" | "significant_regression")
            )
        })
        .count()
}

#[test]
fn fifty_identical_commands_give_false_positives_only_without_correction() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = identical_commands(dir.path(), 50);
    let base = ["--bootstrap-resamples=1000"];
    let plain = export(&base, &fixture);
    assert!(
        significant(&plain) >= 1,
        "the uncorrected family should contain a false positive"
    );
    for method in ["holm", "bonferroni", "bh"] {
        let doc = export(
            &[base.as_slice(), &["--correction", method]].concat(),
            &fixture,
        );
        if method == "bh" {
            // BH controls the false discovery rate, not the chance of any false positive,
            // and the bootstrap p-values differ between 32- and 64-bit targets
            assert!(significant(&doc) <= significant(&plain), "{method}");
        } else {
            assert_eq!(significant(&doc), 0, "{method}");
        }
        assert_eq!(
            doc["perfratio"]["statistics"]["correction"]["method"],
            method
        );
        assert_eq!(
            doc["perfratio"]["statistics"]["correction"]["family_size"],
            49
        );
    }
}

#[test]
fn markers_and_json_agree_on_the_surviving_comparisons() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = identical_commands(dir.path(), 12);
    let doc = export(
        &["--correction", "holm", "--bootstrap-resamples=1000"],
        &fixture,
    );
    let alpha = doc["perfratio"]["statistics"]["correction"]["alpha"]
        .as_f64()
        .unwrap();
    for r in doc["results"].as_array().unwrap() {
        let Some(adjusted) = r.get("p_value_adjusted").and_then(Value::as_f64) else {
            continue;
        };
        let raw = r["p_value"].as_f64().unwrap();
        assert!(adjusted >= raw && adjusted <= 1.0);
        let significant = r["significance"] != "noise";
        assert_eq!(significant, adjusted < alpha, "{r}");
    }
}

#[test]
fn a_real_difference_survives_the_correction() {
    let dir = tempfile::tempdir().unwrap();
    let mut rng = Rng(7);
    let mut results = Vec::new();
    for i in 0..5 {
        let shift = if i == 4 { 0.5 } else { 0.0 };
        let times: Vec<f64> = (0..20).map(|_| rng.sample() + shift).collect();
        results.push(result(&format!("cmd{i}"), &times));
    }
    let path = dir.path().join("shifted.json");
    std::fs::write(&path, json!({ "results": results }).to_string()).unwrap();
    let doc = export(&["--correction", "holm"], &path);
    let slow = doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["command"] == "cmd4")
        .unwrap();
    assert_eq!(slow["significance"], "significant_regression");
    assert!(slow["p_value_adjusted"].as_f64().unwrap() < 0.05);
}

#[test]
fn all_pairs_enlarges_the_family() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = identical_commands(dir.path(), 6);
    let reference = export(
        &["--correction", "holm", "--bootstrap-resamples=1000"],
        &fixture,
    );
    let all = export(
        &["--correction", "holm", "--comparison-family", "all-pairs"],
        &fixture,
    );
    assert_eq!(
        reference["perfratio"]["statistics"]["correction"]["family_size"],
        5
    );
    assert_eq!(
        all["perfratio"]["statistics"]["correction"]["family_size"],
        5 + 10
    );
    assert_eq!(
        all["perfratio"]["statistics"]["correction"]["family"],
        "all-pairs"
    );
}

#[test]
fn default_output_has_no_correction_fields() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = identical_commands(dir.path(), 4);
    let doc = export(&[], &fixture);
    assert!(doc["perfratio"]["statistics"].get("correction").is_none());
    assert!(doc["results"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r.get("p_value_adjusted").is_none()));
    let explicit = export(&["--correction", "none"], &fixture);
    assert_eq!(doc["results"], explicit["results"]);
}

#[test]
fn the_summary_names_method_family_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = identical_commands(dir.path(), 4);
    perfratio()
        .arg("--import-json")
        .arg(&fixture)
        .args(["--correction", "bh", "--style", "basic"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Multiple-comparison correction: bh over 3 comparisons (reference family), alpha = 0.05",
        ))
        .stdout(predicate::str::contains("adjusted p"));
}

#[test]
fn invalid_values_are_rejected() {
    perfratio()
        .args(["--correction", "sidak", "-N", "true"])
        .assert()
        .failure();
    perfratio()
        .args(["--comparison-family", "all-pairs", "-N", "true"])
        .assert()
        .failure();
}
