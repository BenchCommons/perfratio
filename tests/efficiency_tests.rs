mod common;
use common::perfratio;
use predicates::prelude::*;
use serde_json::Value;

/// A result with energy: 2 runs of 2 s and 4 s using 10 J and 20 J.
const FILE: &str = r#"{"metric":"wall","results":[
{"command":"a","mean":3.0,"stddev":1.0,"median":3.0,"user":1.0,"system":0.0,"min":2.0,"max":4.0,
 "times":[2.0,4.0],"exit_codes":[0,0],"mean_energy_joules":15.0,"energy_joules":[10.0,20.0]},
{"command":"b","mean":1.0,"stddev":0.0,"median":1.0,"user":1.0,"system":0.0,"min":1.0,"max":1.0,
 "times":[1.0,1.0],"exit_codes":[0,0],"mean_energy_joules":40.0,"energy_joules":[40.0,40.0]}]}"#;

fn import(args: &[&str]) -> (Value, assert_cmd::assert::Assert) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("in.json");
    std::fs::write(&file, FILE).unwrap();
    let out = dir.path().join("out.json");
    let assert = perfratio()
        .args(["--debug-mode", "--import-json"])
        .arg(&file)
        .args(args)
        .arg("--export-json")
        .arg(&out)
        .assert();
    let json = std::fs::read_to_string(&out)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    (json, assert)
}

#[test]
fn work_adds_derived_metrics_with_kind_derived() {
    let (json, assert) = import(&["--work", "100ops"]);
    assert.success();
    let summary = &json["results"][0]["custom_metrics_summary"];
    assert!((summary["time_per_op"]["mean"].as_f64().unwrap() - 0.03).abs() < 1e-12);
    assert!((summary["energy_per_op"]["mean"].as_f64().unwrap() - 0.15).abs() < 1e-12);
    assert_eq!(summary["ops_per_joule"]["min"], 5.0);
    assert_eq!(summary["edp"]["mean"], 50.0);
    assert_eq!(summary["ed2p"]["mean"], 180.0);
    let metrics = json["perfratio"]["metrics"].as_array().unwrap();
    let find = |id: &str| metrics.iter().find(|m| m["id"] == id).unwrap();
    for id in ["custom.edp", "custom.ed2p", "custom.energy_per_op"] {
        assert_eq!(find(id)["kind"], "derived", "{id}");
    }
    assert_eq!(
        find("custom.ops_per_joule")["direction"],
        "higher_is_better"
    );
    assert_eq!(find("custom.ops_per_joule")["unit"], "ops/J");
    assert_eq!(find("custom.edp")["unit"], "J*s");
}

#[test]
fn derived_metrics_work_with_pareto_and_budget() {
    // b: 1 s, 40 J (EDP 40), a: mean EDP 50; both are on the frontier of (edp, wall)? b is better in both
    let (json, assert) = import(&[
        "--work=100",
        "--pareto=custom:edp,wall",
        "--budget=custom:edp<=45",
    ]);
    assert
        .code(predicates::ord::ne(0))
        .stdout(predicate::str::contains("edp"));
    assert!(json["perfratio"]["compare"].is_object());
    let (_, ok) = import(&["--work=100", "--budget=custom:ops_per_joule>=2"]);
    ok.success();
}

#[test]
fn metric_selects_a_derived_metric() {
    let dir = tempfile::tempdir().unwrap();
    let out = perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--work=1000",
            "--metric=custom:time_per_op",
        ])
        .arg("sleep 0.1")
        .arg("sleep 0.3")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    drop(dir);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("time_per_op"), "{text}");
}

#[test]
fn rejects_bad_use() {
    perfratio()
        .args(["--debug-mode", "--work=0", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be positive"));
    perfratio()
        .args(["--debug-mode", "--metric=custom:time_per_op", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs '--work"));
    perfratio()
        .args(["--debug-mode", "--output-metric=edp=(\\d+)", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("derived efficiency metric"));
}
