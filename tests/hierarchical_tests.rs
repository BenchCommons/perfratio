//! `--ci-method`, `--paired` and `results[].hierarchy` (#368). `--debug-mode`
//! simulates the commands, so the structure is deterministic.

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::Value;

fn export(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.json");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&out)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

#[test]
fn a_plain_sequence_of_runs_has_no_hierarchy() {
    let doc = export(&["--runs=6", "sleep 0.1"]);
    assert!(doc["results"][0].get("hierarchy").is_none());
    assert_eq!(doc["perfratio"]["statistics"]["ci_method"], "flat");
    assert!(doc["perfratio"]["statistics"].get("paired").is_none());
    assert!(doc["results"][0].get("paired").is_none());
}

#[test]
fn jobs_record_the_worker_of_every_run() {
    let doc = export(&[
        "--runs=12",
        "--jobs=3",
        "--ci-method=hierarchical",
        "sleep 0.1",
    ]);
    let h = &doc["results"][0]["hierarchy"];
    assert_eq!(h["levels"], serde_json::json!(["worker", "run"]));
    assert_eq!(h["cluster_level"], "worker");
    let workers = h["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 12, "one worker per run");
    // the initial measurement runs on the main thread
    assert!(workers[0].is_null());
    assert!(workers[1..]
        .iter()
        .all(|w| w.as_u64().is_some_and(|w| w < 3)));
    assert_eq!(h["units"], 12);
    assert_eq!(h["mean_ci"]["method"], "hierarchical");
    assert_eq!(doc["perfratio"]["statistics"]["ci_method"], "hierarchical");
}

#[test]
fn concurrency_makes_the_instances_the_units_of_a_run() {
    let doc = export(&["--runs=5", "--concurrency=3", "sleep 0.1"]);
    let h = &doc["results"][0]["hierarchy"];
    assert_eq!(h["levels"], serde_json::json!(["run", "instance"]));
    assert_eq!(h["cluster_level"], "run");
    assert_eq!(h["clusters"], 5);
    assert_eq!(h["units"], 15);
    // the default method is flat, both intervals are still exported
    assert_eq!(h["mean_ci"]["method"], "flat");
    assert_eq!(h["mean_ci"]["flat"].as_array().unwrap().len(), 2);
    assert_eq!(h["mean_ci"]["hierarchical"].as_array().unwrap().len(), 2);
    assert!(h["variance"]["design_effect"].as_f64().unwrap() >= 1.0);
}

#[test]
fn the_hierarchical_summary_line_is_printed_on_request() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--concurrency=3",
            "--ci-method=hierarchical",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hierarchical 95% CI"));
    perfratio()
        .args(["--debug-mode", "--runs=5", "--concurrency=3", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Hierarchical").not());
}

#[test]
fn paired_compares_the_rounds_of_an_interleaved_schedule() {
    for schedule in ["round-robin", "randomized", "balanced"] {
        let doc = export(&[
            "--runs=8",
            &format!("--schedule={schedule}"),
            "--paired",
            "sleep 0.1",
            "sleep 0.2",
        ]);
        assert_eq!(doc["perfratio"]["statistics"]["paired"], true);
        assert!(doc["results"][0].get("paired").is_none(), "the reference");
        let p = &doc["results"][1]["paired"];
        assert_eq!(p["reference"], "sleep 0.1", "{schedule}");
        assert_eq!(p["rounds"], 8);
        assert!((p["mean_difference"].as_f64().unwrap() - 0.1).abs() < 1e-9);
        assert_eq!(p["p_value"], 0.0);
    }
}

#[test]
fn paired_is_printed_after_the_summary() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=6",
            "--schedule=balanced",
            "--paired",
            "sleep 0.1",
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Paired by round"))
        .stdout(predicate::str::contains("6 rounds"));
}

#[test]
fn paired_needs_an_interleaved_schedule() {
    perfratio()
        .args(["--debug-mode", "--paired", "sleep 0.1", "sleep 0.2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--paired"))
        .stderr(predicate::str::contains("interleaved schedule"));
}

#[test]
fn an_unknown_ci_method_is_rejected() {
    perfratio()
        .args(["--debug-mode", "--ci-method=bayes", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--ci-method"));
}
