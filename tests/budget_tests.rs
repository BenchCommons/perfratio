//! `--budget`, `--budget-file`, `--on-unavailable` (#325).

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn baseline(dir: &Path) -> PathBuf {
    let path = dir.join("baseline.json");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--export-json"])
        .arg(&path)
        .args(["-n", "app", "sleep 0.1"])
        .assert()
        .success();
    path
}

/// `app` at 200 ms instead of 100 ms; extra arguments first
fn run(base: &Path, extra: &[&str]) -> assert_cmd::assert::Assert {
    perfratio()
        .args(["--debug-mode", "--runs=5"])
        .args(extra)
        .arg("--compare")
        .arg(base)
        .args(["-n", "app", "sleep 0.2"])
        .assert()
}

#[test]
fn several_budgets_one_failure_gives_exit_code_3() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let diff = dir.path().join("diff.md");
    run(
        &base,
        &[
            "--budget=wall=+150%",
            "--budget=wall<=150ms",
            "--budget",
            "wall=+5%",
            "--export-diff-markdown",
            diff.to_str().unwrap(),
        ],
    )
    .code(3)
    .stdout(predicate::str::contains("Budgets: fail"))
    .stdout(predicate::str::contains("FAIL"))
    .stderr(predicate::str::contains("2 budget(s) failed"));
    let markdown = std::fs::read_to_string(diff).unwrap();
    assert!(markdown.contains("**Budgets: fail**"), "{markdown}");
    assert!(markdown.contains("**FAIL**"), "{markdown}");
}

#[test]
fn budgets_within_their_limits_pass() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    run(&base, &["--budget=wall=+150%", "--budget=wall<=250ms"])
        .success()
        .stdout(predicate::str::contains("Budgets: pass"));
}

#[test]
fn absolute_budget_works_without_a_baseline_and_relative_needs_one() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--budget=wall<=50ms",
            "sleep 0.1",
        ])
        .assert()
        .code(3);
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--budget=wall<=500ms",
            "sleep 0.1",
        ])
        .assert()
        .success();
    perfratio()
        .args(["--debug-mode", "--runs=5", "--budget=wall=+5%", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a baseline"));
}

#[test]
fn invalid_budgets_are_rejected_before_benchmarking() {
    perfratio()
        .args(["--debug-mode", "--runs=5", "--budget=wall<5%", "sleep 0.1"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("invalid budget"));
}

#[test]
fn unavailable_metrics_follow_on_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    // debug mode measures no energy: in neither run
    for (policy, code, shown) in [("fail", 3, true), ("warn", 0, true), ("skip", 0, false)] {
        let out = run(&base, &["--budget=energy=+3%", "--on-unavailable", policy]).code(code);
        let out = out.get_output();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            stdout.contains("not evaluated"),
            shown,
            "{policy}\n{stdout}"
        );
        assert!(
            !stdout.contains("Budgets: pass"),
            "never a silent pass: {policy}\n{stdout}"
        );
    }
}

#[test]
fn json_compare_block_and_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let out = dir.path().join("out.json");
    run(
        &base,
        &[
            "--budget=wall=+5%",
            "--budget=energy=+3%",
            "--export-json",
            out.to_str().unwrap(),
        ],
    )
    .code(3);
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    let compare = &doc["perfratio"]["compare"];
    assert_eq!(compare["verdict"], "fail");
    assert_eq!(compare["on_unavailable"], "warn");
    assert_eq!(compare["baseline"], base.to_str().unwrap());
    let budgets = compare["budgets"].as_array().unwrap();
    assert_eq!(budgets.len(), 2);
    assert_eq!(budgets[0]["metric"], "wall");
    assert_eq!(budgets[0]["kind"], "relative");
    assert_eq!(budgets[0]["op"], "<=");
    assert_eq!(budgets[0]["limit"], 0.05);
    assert_eq!(budgets[0]["verdict"], "fail");
    assert!((budgets[0]["change"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    assert!((budgets[0]["baseline"].as_f64().unwrap() - 0.1).abs() < 1e-9);
    assert!((budgets[0]["current"].as_f64().unwrap() - 0.2).abs() < 1e-9);
    assert_eq!(budgets[1]["verdict"], "not_evaluated");
    // unavailable is not written as 0
    assert!(budgets[1].get("current").is_none());
    assert!(budgets[1].get("change").is_none());
}

#[test]
fn fail_if_regressed_is_the_wall_budget_shorthand() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let out = dir.path().join("out.json");
    run(
        &base,
        &[
            "--fail-if-regressed=5%",
            "--export-json",
            out.to_str().unwrap(),
        ],
    )
    .code(3)
    .stderr(predicate::str::contains("1 benchmark(s) regressed"));
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    assert_eq!(doc["perfratio"]["compare"]["budgets"][0]["verdict"], "fail");
}

#[test]
fn budget_files_toml_and_json() {
    let dir = tempfile::tempdir().unwrap();
    let base = baseline(dir.path());
    let toml = dir.path().join("budgets.toml");
    std::fs::write(
        &toml,
        "# gates\nbudgets = [\"wall=+150%\", \"wall<=150ms\"]\n",
    )
    .unwrap();
    run(&base, &["--budget-file", toml.to_str().unwrap()])
        .code(3)
        .stderr(predicate::str::contains("1 budget(s) failed"));
    let json = dir.path().join("budgets.json");
    std::fs::write(&json, r#"{"budgets": ["wall=+150%", "wall<=250ms"]}"#).unwrap();
    run(&base, &["--budget-file", json.to_str().unwrap()]).success();
    let bad = dir.path().join("bad.toml");
    std::fs::write(&bad, "budgets = 3\n").unwrap();
    run(&base, &["--budget-file", bad.to_str().unwrap()])
        .failure()
        .stderr(predicate::str::contains("bad.toml"));
}
