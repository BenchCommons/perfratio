//! `--verdict-json` (#386): the verdict document of a `--compare` run. The mock executor
//! (`--debug-mode`) simulates `sleep <seconds> [user] [system] [memory]`, so a baseline and a
//! current run can differ in wall time and memory independently.

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn baseline(dir: &Path) -> PathBuf {
    let path = dir.join("baseline.json");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--style=none", "--export-json"])
        .arg(&path)
        .args([
            "-n",
            "app",
            "sleep 0.1 0 0 1000",
            "-n",
            "other",
            "sleep 0.05 0 0 1000",
        ])
        .assert()
        .success();
    path
}

/// Verdict of a run of `app` and `other` against the baseline, with extra options.
fn verdict(app: &str, other: &str, extra: &[&str]) -> (Value, assert_cmd::assert::Assert) {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let out = dir.path().join("verdict.json");
    let assert = perfratio()
        .args(["--debug-mode", "--runs=5", "--style=none", "--compare"])
        .arg(&base)
        .arg("--verdict-json")
        .arg(&out)
        .args(extra)
        .args(["-n", "app", app, "-n", "other", other])
        .assert();
    let document = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    (document, assert)
}

fn metric<'a>(doc: &'a Value, command: &str, name: &str) -> &'a Value {
    doc["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["command"] == command)
        .unwrap_or_else(|| panic!("no command {command}"))["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["metric"] == name)
        .unwrap_or_else(|| panic!("no metric {name}"))
}

fn command<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["command"] == name)
        .unwrap()
}

const SAME_APP: &str = "sleep 0.1 0 0 1000";
const SAME_OTHER: &str = "sleep 0.05 0 0 1000";

#[test]
fn an_unchanged_run_is_unchanged() {
    let (doc, assert) = verdict(SAME_APP, SAME_OTHER, &[]);
    assert.success();
    assert_eq!(doc["verdictVersion"], "1.0");
    assert_eq!(doc["verdict"], "unchanged");
    assert_eq!(doc["summary"]["unchanged"], 2);
    let wall = metric(&doc, "app", "wall");
    assert_eq!(wall["assessment"], "unchanged");
    assert_eq!(wall["change"], 0.0);
    assert_eq!(wall["direction"], "lower_is_better");
    assert_eq!(wall["runs"], json!({"baseline": 5, "current": 5}));
    assert!(doc.get("gate").is_none());
}

#[test]
fn the_first_key_is_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let out = dir.path().join("v.json");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--style=none", "--compare"])
        .arg(&base)
        .arg("--verdict-json")
        .arg(&out)
        .arg("sleep 0.1")
        .assert()
        .success();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(
        text.starts_with("{\n  \"verdictVersion\": \"1.0\","),
        "{text}"
    );
}

#[test]
fn a_faster_command_is_improved_with_its_uncertainty() {
    let (doc, assert) = verdict("sleep 0.05 0 0 1000", SAME_OTHER, &[]);
    assert.success();
    assert_eq!(doc["verdict"], "improved");
    let wall = metric(&doc, "app", "wall");
    assert_eq!(wall["assessment"], "improved");
    assert!((wall["change"].as_f64().unwrap() + 0.5).abs() < 1e-9);
    assert!(wall["ci_low"].as_f64().unwrap() <= wall["change"].as_f64().unwrap());
    assert!(wall["ci_high"].as_f64().unwrap() >= wall["change"].as_f64().unwrap());
    assert!(wall["p_value"].as_f64().unwrap() < 0.05);
    assert_eq!(command(&doc, "app")["verdict"], "improved");
    assert_eq!(command(&doc, "other")["verdict"], "unchanged");
    assert_eq!(doc["summary"]["improved"], 1);
}

#[test]
fn a_slower_command_is_regressed() {
    let (doc, assert) = verdict("sleep 0.2 0 0 1000", SAME_OTHER, &[]);
    assert.success();
    assert_eq!(doc["verdict"], "regressed");
    assert_eq!(metric(&doc, "app", "wall")["assessment"], "regressed");
}

#[test]
fn faster_but_hungrier_is_a_trade_off() {
    let (doc, assert) = verdict("sleep 0.05 0 0 2000", SAME_OTHER, &[]);
    assert.success();
    assert_eq!(metric(&doc, "app", "wall")["assessment"], "improved");
    assert_eq!(metric(&doc, "app", "memory")["assessment"], "regressed");
    assert_eq!(command(&doc, "app")["verdict"], "trade_off");
    assert_eq!(doc["verdict"], "trade_off");
    assert_eq!(doc["summary"]["trade_off"], 1);
}

#[test]
fn a_budget_that_fails_is_in_the_gate_and_the_file_is_still_written() {
    let (doc, assert) = verdict(
        "sleep 0.2 0 0 1000",
        SAME_OTHER,
        &["--budget", "wall=+5%", "--budget", "memory<=2000"],
    );
    assert
        .code(3)
        .stderr(predicate::str::contains("budget(s) failed"));
    assert_eq!(doc["gate"]["verdict"], "fail");
    assert_eq!(doc["gate"]["failures"], 1);
    let budgets = command(&doc, "app")["budgets"].as_array().unwrap();
    assert_eq!(budgets.len(), 2);
    let wall = budgets.iter().find(|b| b["metric"] == "wall").unwrap();
    assert_eq!(wall["verdict"], "fail");
    let memory = budgets.iter().find(|b| b["metric"] == "memory").unwrap();
    assert_eq!(memory["verdict"], "pass");
}

#[test]
fn a_passing_gate_and_budgets_of_unmatched_commands() {
    let (doc, assert) = verdict(SAME_APP, SAME_OTHER, &["--budget", "memory<=2000"]);
    assert.success();
    assert_eq!(doc["gate"]["verdict"], "pass");
    assert_eq!(doc["gate"]["failures"], 0);
    assert!(doc["gate"].get("other_budgets").is_none());

    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let out = dir.path().join("v.json");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--style=none", "--compare"])
        .arg(&base)
        .arg("--verdict-json")
        .arg(&out)
        .args([
            "--budget",
            "memory<=2000",
            "-n",
            "added",
            "sleep 0.05 0 0 10",
        ])
        .assert()
        .success();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(doc["new"], json!(["added"]));
    assert_eq!(doc["removed"], json!(["app", "other"]));
    assert_eq!(doc["commands"], json!([]));
    assert_eq!(doc["verdict"], "not_evaluated");
    assert_eq!(doc["gate"]["other_budgets"][0]["command"], "added");
}

#[test]
fn too_few_runs_are_inconclusive_not_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let out = dir.path().join("v.json");
    perfratio()
        .args(["--debug-mode", "--runs=2", "--style=none", "--compare"])
        .arg(&base)
        .arg("--verdict-json")
        .arg(&out)
        .args(["-n", "app", "sleep 0.2 0 0 1000"])
        .assert()
        .success();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let wall = metric(&doc, "app", "wall");
    assert_eq!(wall["assessment"], "inconclusive");
    assert_eq!(wall["reason"], "too_few_runs");
    assert_eq!(doc["verdict"], "inconclusive");
}

#[test]
fn a_metric_missing_on_one_side_is_not_evaluated() {
    // the baseline was measured without --energy
    let (doc, assert) = verdict(SAME_APP, SAME_OTHER, &["--energy"]);
    assert.success();
    let energy = metric(&doc, "app", "energy");
    assert_eq!(energy["assessment"], "not_evaluated");
    assert_eq!(energy["reason"], "unavailable_in_baseline");
    assert!(energy.get("change").is_none());
    assert!(energy["current"].as_f64().is_some());
    // it does not hide the other metrics
    assert_eq!(metric(&doc, "app", "wall")["assessment"], "unchanged");
    assert_eq!(doc["verdict"], "unchanged");
}

#[test]
fn the_noise_threshold_decides_what_counts_as_a_change() {
    let (doc, assert) = verdict(
        "sleep 0.05 0 0 1000",
        SAME_OTHER,
        &["--noise-threshold", "60%"],
    );
    assert.success();
    assert_eq!(metric(&doc, "app", "wall")["assessment"], "unchanged");
    assert_eq!(doc["settings"]["noise_threshold"], 0.6);
}

#[test]
fn verdict_json_needs_compare() {
    perfratio()
        .args([
            "--debug-mode",
            "--verdict-json",
            "/tmp/never.json",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--compare"));
}

#[test]
fn the_document_matches_its_schema() {
    let schema: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/docs/schema/verdict.schema.json"
        ))
        .unwrap(),
    )
    .unwrap();
    // strict copy: a field the schema does not describe fails
    fn strict(v: &mut Value) {
        match v {
            Value::Object(map) => {
                if map.contains_key("properties") && !map.contains_key("additionalProperties") {
                    map.insert("additionalProperties".into(), json!(false));
                }
                map.values_mut().for_each(strict);
            }
            Value::Array(items) => items.iter_mut().for_each(strict),
            _ => {}
        }
    }
    let mut strict_schema = schema.clone();
    strict(&mut strict_schema);
    let validator = jsonschema::validator_for(&strict_schema).unwrap();
    for (app, extra) in [
        ("sleep 0.05 0 0 2000", vec!["--budget", "wall=+5%"]),
        (SAME_APP, vec!["--equivalence-margin", "5%"]),
        (
            "sleep 0.2 0 0 1000",
            vec!["--budget", "memory<=2000", "--on-unavailable", "skip"],
        ),
    ] {
        let (doc, _) = verdict(app, SAME_OTHER, &extra);
        let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{errors:?}\n{doc:#}");
    }
}

#[test]
fn equivalence_is_reported_with_a_margin() {
    let (doc, assert) = verdict(SAME_APP, SAME_OTHER, &["--equivalence-margin", "5%"]);
    assert.success();
    let wall = metric(&doc, "app", "wall");
    assert_eq!(wall["assessment"], "equivalent");
    assert!(wall["equivalence_p_value"].as_f64().unwrap() < 0.05);
    assert_eq!(doc["verdict"], "unchanged");
}
