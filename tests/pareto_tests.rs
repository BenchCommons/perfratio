//! `--pareto`, `--constraint`, `--objective` (#326)

mod common;
use common::perfratio;

use predicates::prelude::*;

fn debug() -> assert_cmd::Command {
    let mut cmd = perfratio();
    cmd.arg("--debug-mode");
    cmd
}

#[test]
fn prints_the_frontier_section() {
    debug()
        .args(["--runs=3", "--pareto=wall,memory", "sleep 0.1", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Pareto frontier over wall↓, memory↓",
        ))
        .stdout(predicate::str::contains("frontier"));
}

#[test]
fn absent_without_the_flag() {
    debug()
        .args(["--runs=3", "sleep 0.1", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Pareto").not());
}

#[test]
fn unavailable_metric_is_excluded_not_zero() {
    debug()
        .args(["--runs=3", "--pareto=wall,energy", "sleep 0.1", "sleep 0.2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("excluded: no data for energy"));
}

#[test]
fn constraint_excludes_and_objective_picks() {
    debug()
        .args([
            "--runs=3",
            "--pareto=wall",
            "--constraint=wall<=1min",
            "--constraint=wall<=50ms",
            "--objective=minimize:wall",
            "sleep 0.1",
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("violates 'wall<=50ms'"))
        .stdout(predicate::str::contains(
            "no command satisfies the constraints",
        ));
}

#[test]
fn invalid_values_fail_before_benchmarking() {
    for args in [
        vec!["--pareto=bogus"],
        vec!["--pareto=wall,wall"],
        vec!["--pareto=wall", "--constraint=wall"],
        vec!["--pareto=wall", "--constraint=wall<=5MiB"],
        vec!["--constraint=wall<=1s"],
        vec!["--pareto=wall", "--objective=best:wall"],
        vec!["--pareto=wall", "--noise-threshold=-3"],
    ] {
        debug()
            .args(args)
            .arg("sleep 0.1")
            .assert()
            .failure()
            .stderr(predicate::str::contains("Error").or(predicate::str::contains("error")));
    }
}

#[test]
fn json_and_markdown_exports() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    let md = dir.path().join("out.md");
    let html = dir.path().join("out.html");
    debug()
        .args([
            "--runs=3",
            "--pareto=wall,memory",
            "--objective=minimize:wall",
        ])
        .arg(format!("--export-json={}", json.display()))
        .arg(format!("--export-markdown={}", md.display()))
        .arg(format!("--export-html={}", html.display()))
        .args(["sleep 0.1", "sleep 0.2"])
        .assert()
        .success();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(doc["schemaVersion"], "1.18");
    assert_eq!(doc["pareto"]["metrics"][0]["metric"], "wall");
    assert!(doc["pareto"]["frontier"]
        .as_array()
        .is_some_and(|f| !f.is_empty()));
    assert_eq!(doc["pareto"]["objective"]["metric"], "wall");
    let md = std::fs::read_to_string(&md).unwrap();
    assert!(
        md.contains("### Pareto frontier over wall↓, memory↓"),
        "{md}"
    );
    let html = std::fs::read_to_string(&html).unwrap();
    assert!(
        html.contains("<h2>Pareto frontier over wall↓, memory↓</h2>"),
        "{html}"
    );
}

#[test]
fn json_has_no_pareto_object_without_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    debug()
        .args(["--runs=3"])
        .arg(format!("--export-json={}", json.display()))
        .arg("sleep 0.1")
        .assert()
        .success();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert!(doc.get("pareto").is_none());
}
