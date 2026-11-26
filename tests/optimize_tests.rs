//! `--optimize` (#98). The mock executor (`--debug-mode`) only understands
//! `sleep <seconds> [user] [system] [memory]`, so the cost is monotonic in the
//! parameter here; the shape of the search is covered by unit tests in `src/optimize.rs`.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(["--debug-mode", "--runs=3", "--style=none", "--export-json"])
        .arg(&path)
        .args(args)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn finds_boundary_minimum_and_exports_every_point() {
    let json = export(&["--optimize", "n", "1", "40", "sleep {n}"]);
    let optimize = &json["optimize"];
    assert_eq!(optimize["var"], "n");
    assert_eq!(optimize["best"], 1);
    assert_eq!(optimize["best_command"], "sleep 1");
    assert_eq!(optimize["algorithm"], "golden-section");
    assert_eq!(optimize["at_boundary"], true);
    assert_eq!(optimize["metric"], "wall");
    let evaluated = optimize["evaluated"].as_array().unwrap();
    assert!(evaluated.len() <= 12, "{evaluated:?}");
    assert!(evaluated.len() < 40);
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), evaluated.len());
    for (result, value) in results.iter().zip(evaluated) {
        assert_eq!(
            result["parameters"]["n"],
            value.to_string().trim_matches('"')
        );
    }
    assert!(json["schemaVersion"].as_str().unwrap().starts_with("1."));
}

#[test]
fn budget_limits_evaluations() {
    let json = export(&[
        "--optimize",
        "n",
        "1",
        "1000",
        "--optimize-budget",
        "4",
        "sleep {n}",
    ]);
    assert!(json["optimize"]["evaluated"].as_array().unwrap().len() <= 4);
}

#[test]
fn range_within_budget_is_exhaustive() {
    let json = export(&["--optimize", "n", "1", "5", "sleep {n}"]);
    assert_eq!(json["optimize"]["algorithm"], "exhaustive");
    assert_eq!(json["optimize"]["evaluations"], 5);
}

#[test]
fn single_value_is_evaluated() {
    let json = export(&["--optimize", "n", "7", "7", "sleep {n}"]);
    assert_eq!(json["optimize"]["best"], 7);
    assert_eq!(json["results"].as_array().unwrap().len(), 1);
    assert!(json["optimize"].get("runner_up").is_none());
}

#[test]
fn memory_metric_is_minimized() {
    let json = export(&[
        "--metric",
        "memory",
        "--optimize",
        "n",
        "1",
        "30",
        "sleep 0.1 0 0 {n}",
    ]);
    assert_eq!(json["optimize"]["metric"], "memory");
    assert_eq!(json["optimize"]["best"], 1);
}

#[test]
fn energy_metric_runs_in_debug_mode() {
    let json = export(&[
        "--metric",
        "energy",
        "--optimize",
        "n",
        "1",
        "30",
        "sleep {n}",
    ]);
    assert_eq!(json["optimize"]["metric"], "energy");
    // the mock reports the same energy for every value: a plateau, the lowest value wins
    assert_eq!(json["optimize"]["best"], 1);
}

#[test]
fn other_parameters_get_one_search_each() {
    let json = export(&[
        "--optimize",
        "n",
        "1",
        "30",
        "-L",
        "k",
        "1,2",
        "sleep {n}.{k}",
    ]);
    let searches = json["optimize"].as_array().unwrap();
    assert_eq!(searches.len(), 2);
    assert_eq!(searches[0]["fixed"]["k"], "1");
    assert_eq!(searches[1]["fixed"]["k"], "2");
    assert_eq!(searches[1]["best_command"], "sleep 1.2");
}

#[test]
fn terminal_summary_lists_best_point() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--style=basic",
            "--optimize",
            "n",
            "1",
            "30",
            "sleep {n}",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Optimization of n ∈ [1, 30] for wall",
        ))
        .stdout(predicate::str::contains("← best"))
        .stdout(predicate::str::contains("Best: n=1"));
}

fn fails_with(args: &[&str], message: &str) {
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains(message));
}

#[test]
fn validation_errors() {
    fails_with(
        &["--optimize", "n", "1", "9", "sleep 1"],
        "does not contain '{n}'",
    );
    fails_with(
        &["--optimize", "n", "9", "1", "sleep {n}"],
        "MIN (9) must not be greater than MAX (1)",
    );
    fails_with(
        &["--optimize", "n", "1", "x", "sleep {n}"],
        "is not an integer",
    );
    fails_with(
        &["--optimize", "n", "1", "1.5", "sleep {n}"],
        "is not an integer",
    );
    fails_with(
        &[
            "--optimize",
            "n",
            "1",
            "9",
            "--optimize-budget",
            "2",
            "sleep {n}",
        ],
        "must be at least 3",
    );
    fails_with(
        &[
            "--optimize",
            "n",
            "1",
            "9",
            "-P",
            "n",
            "1",
            "3",
            "sleep {n}",
        ],
        "same variable 'n'",
    );
    fails_with(
        &["--optimize", "n", "1", "9", "-L", "n", "1,2", "sleep {n}"],
        "same variable 'n'",
    );
    fails_with(
        &[
            "--optimize",
            "n",
            "1",
            "9",
            "--round-robin",
            "sleep {n}",
            "sleep {n}",
        ],
        "round-robin",
    );
    fails_with(
        &["--optimize-budget", "5", "sleep 1"],
        "requires '--optimize'",
    );
}

#[test]
fn json_export_without_optimize_has_no_optimize_key() {
    let json = export(&["sleep 0.1"]);
    assert!(json.get("optimize").is_none());
}
