//! `--optimize` over several variables and the random, halving and Bayesian algorithms (#385).
//! The mock executor (`--debug-mode`) only simulates `sleep <seconds> [user] [system] [memory]`.
//! `sleep 0.0{a}{b}` makes the wall time depend on both variables in the order of (a, b), so
//! the minimum is at the lowest values; the shape of the searches is covered by the unit
//! tests on synthetic surfaces in `src/search.rs`.

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

const TWO: &[&str] = &["--optimize", "a", "1", "6", "--optimize", "b", "1", "6"];

fn with(extra: &[&str]) -> Vec<String> {
    TWO.iter()
        .chain(extra)
        .chain(&["sleep 0.0{a}{b}"])
        .map(|s| s.to_string())
        .collect()
}

fn json_for(extra: &[&str]) -> serde_json::Value {
    let args = with(extra);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    export(&refs)
}

#[test]
fn two_variables_are_searched_and_every_point_is_a_result() {
    let json = json_for(&["--optimize-budget", "8"]);
    let o = &json["optimize"];
    assert_eq!(o["algorithm"], "random");
    assert_eq!(o["evaluations"], 8);
    assert_eq!(o["vars"].as_array().unwrap().len(), 2);
    assert_eq!(o["vars"][0]["name"], "a");
    assert_eq!(o["evaluated_points"].as_array().unwrap().len(), 8);
    assert_eq!(json["results"].as_array().unwrap().len(), 8);
    // the required one-variable fields describe the first declared variable
    assert_eq!(o["var"], "a");
    assert_eq!((o["min"].as_i64(), o["max"].as_i64()), (Some(1), Some(6)));
    assert_eq!(o["best"], o["best_point"]["a"]);
    assert_eq!(o["unimodal_warning"], false);
    let evaluated: Vec<_> = o["evaluated_points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["a"].clone())
        .collect();
    assert_eq!(o["evaluated"].as_array().unwrap(), &evaluated);
    let best = &o["best_point"];
    let command = o["best_command"].as_str().unwrap();
    assert_eq!(
        command,
        format!("sleep 0.0{}{}", best["a"], best["b"]),
        "best_command matches best_point"
    );
    // the best of the evaluated points
    let fastest = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["mean"].as_f64().unwrap())
        .fold(f64::INFINITY, f64::min);
    assert!((o["best_value"].as_f64().unwrap() - fastest).abs() < 1e-9);
    assert_eq!(o["cost"]["points"], 8);
    assert_eq!(o["cost"]["runs"], 24);
}

#[test]
fn the_same_seed_gives_the_same_search() {
    let one = json_for(&["--optimize-budget", "6", "--seed", "3"]);
    let two = json_for(&["--optimize-budget", "6", "--seed", "3"]);
    assert_eq!(
        one["optimize"]["evaluated_points"],
        two["optimize"]["evaluated_points"]
    );
    let other = json_for(&["--optimize-budget", "6", "--seed", "4"]);
    assert_ne!(
        one["optimize"]["evaluated_points"],
        other["optimize"]["evaluated_points"]
    );
}

#[test]
fn a_grid_inside_the_budget_is_exhaustive_and_finds_the_minimum() {
    for algorithm in ["random", "halving", "bayes"] {
        let json = export(&[
            "--optimize",
            "a",
            "1",
            "3",
            "--optimize",
            "b",
            "1",
            "3",
            "--optimize-algorithm",
            algorithm,
            "--optimize-budget",
            "12",
            "sleep 0.0{a}{b}",
        ]);
        let o = &json["optimize"];
        assert_eq!(o["algorithm"], "exhaustive", "{algorithm}");
        assert_eq!(o["evaluations"], 9);
        assert_eq!(o["best_point"]["a"], 1);
        assert_eq!(o["best_point"]["b"], 1);
        assert_eq!(o["at_boundary"], true);
    }
}

#[test]
fn halving_reports_its_rungs_and_keeps_one_result_per_point() {
    let json = json_for(&["--optimize-algorithm", "halving", "--optimize-budget", "9"]);
    let o = &json["optimize"];
    assert_eq!(o["algorithm"], "successive-halving");
    let rungs = o["cost"]["rungs"].as_array().unwrap();
    // 9 points, the best third, then the last two
    assert_eq!(rungs.len(), 3);
    assert_eq!(rungs[0]["points"], 9);
    assert_eq!(rungs[1]["points"], 3);
    assert_eq!(rungs[2]["points"], 2);
    assert_eq!(rungs[0]["runs"], 3);
    assert_eq!(rungs[1]["runs"], 9);
    assert_eq!(rungs[2]["runs"], 27);
    assert_eq!(o["evaluations"], 9);
    assert_eq!(json["results"].as_array().unwrap().len(), 9);
    assert_eq!(o["cost"]["benchmarks"], 14);
    assert_eq!(o["cost"]["runs"], 9 * 3 + 3 * 9 + 2 * 27);
}

#[test]
fn bayes_stays_within_the_budget() {
    let json = json_for(&["--optimize-algorithm", "bayes", "--optimize-budget", "7"]);
    assert_eq!(json["optimize"]["algorithm"], "bayesian");
    assert_eq!(json["optimize"]["evaluations"], 7);
}

#[test]
fn validation_runs_a_fresh_round_without_adding_results() {
    let json = json_for(&["--optimize-budget", "8", "--optimize-validate"]);
    let v = &json["optimize"]["validation"];
    assert!(["confirmed", "refuted", "inconclusive"].contains(&v["status"].as_str().unwrap()));
    assert_eq!(v["runs"], 6);
    assert_eq!(json["results"].as_array().unwrap().len(), 8);
}

#[test]
fn one_variable_with_another_algorithm_keeps_the_golden_fields() {
    let json = export(&[
        "--optimize",
        "n",
        "1",
        "40",
        "--optimize-algorithm",
        "random",
        "--optimize-budget",
        "5",
        "sleep 0.0{n}",
    ]);
    let o = &json["optimize"];
    assert_eq!(o["var"], "n");
    assert_eq!(o["min"], 1);
    assert_eq!(o["max"], 40);
    assert_eq!(o["evaluated"].as_array().unwrap().len(), 5);
    assert_eq!(o["best"], o["best_point"]["n"]);
}

#[test]
fn the_default_search_of_one_variable_is_unchanged() {
    let json = export(&["--optimize", "n", "1", "40", "sleep {n}"]);
    assert_eq!(json["optimize"]["algorithm"], "golden-section");
    assert!(json["optimize"].get("vars").is_none());
}

fn fails(args: &[&str], message: &str) {
    perfratio()
        .args(["--debug-mode", "--runs=3", "--style=none"])
        .args(args)
        .assert()
        .failure()
        .stderr(predicate::str::contains(message));
}

#[test]
fn invalid_combinations_are_rejected() {
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "5",
            "--optimize-algorithm",
            "golden",
            "sleep 0.0{a}{b}",
        ],
        "searches one variable",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "a",
            "1",
            "5",
            "sleep {a}",
        ],
        "given twice",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "5",
            "sleep {a}",
        ],
        "does not contain '{b}'",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "x",
            "sleep 0.0{a}{b}",
        ],
        "not an integer",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "5",
            "--optimize-budget",
            "2",
            "sleep 0.0{a}{b}",
        ],
        "at least 3",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "5",
            "--optimize-confirm",
            "1",
            "sleep 0.0{a}{b}",
        ],
        "belongs to the golden-section",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize-validate",
            "sleep {a}",
        ],
        "'--optimize-validate' needs",
    );
    fails(
        &["--optimize-algorithm", "random", "sleep 1"],
        "requires '--optimize'",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "5",
            "-D",
            "0.5",
            "sleep 0.0{a}{b}",
        ],
        "('-D')",
    );
    fails(
        &[
            "--optimize",
            "a",
            "1",
            "5",
            "--optimize",
            "b",
            "1",
            "5",
            "-L",
            "a",
            "1,2",
            "sleep 0.0{a}{b}",
        ],
        "conflicts with '--parameter-list'",
    );
}

#[test]
fn another_parameter_combines_with_the_search() {
    let json = export(&[
        "-L",
        "c",
        "0,1",
        "--optimize",
        "a",
        "1",
        "6",
        "--optimize",
        "b",
        "1",
        "6",
        "--optimize-budget",
        "4",
        "sleep 0.0{a}{b}{c}",
    ]);
    let searches = json["optimize"].as_array().unwrap();
    assert_eq!(searches.len(), 2);
    assert_eq!(searches[0]["fixed"]["c"], "0");
    assert_eq!(searches[1]["fixed"]["c"], "1");
}

/// A multi-variable export validates against the strict 1.45 schema, which still requires the
/// fields 1.44 required.
#[test]
fn a_multi_variable_export_validates_against_the_1_45_schema() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/schema/1.45/perfratio-export.schema.json"
    );
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for extra in [
        &["--optimize-budget", "8"][..],
        &["--optimize-algorithm", "halving"],
    ] {
        let json = json_for(extra);
        let errors: Vec<String> = validator
            .iter_errors(&json)
            .map(|e| format!("{e} at {}", e.instance_path()))
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
    }
    for key in ["var", "min", "max", "best", "unimodal_warning", "evaluated"] {
        let mut json = json_for(&[]);
        json["optimize"].as_object_mut().unwrap().remove(key);
        assert!(!validator.is_valid(&json), "`{key}` must be required");
    }
}
