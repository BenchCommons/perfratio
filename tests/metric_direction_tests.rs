//! `--output-metric NAME[unit,higher]=REGEX`, direction-aware ranking and descriptors in
//! `--import-json` (#317).

mod common;
use common::perfratio;

use predicates::prelude::*;

fn json_of(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .args(["--export-json", path.to_str().unwrap()])
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn descriptor<'a>(json: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    json["perfratio"]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap_or_else(|| panic!("no descriptor {id}"))
}

const RUN: [&str; 9] = [
    "--runs=2",
    "--warmup=0",
    "--output-metric",
    "qps[ops/s,higher]=qps=([0-9.]+)",
    "--metric",
    "custom:qps",
    "--style=basic",
    "echo qps=100",
    "echo qps=250",
];

#[test]
fn higher_is_better_metric_ranks_the_largest_value_first() {
    let json = json_of(&RUN);
    let (low, high) = (&json["results"][0], &json["results"][1]);
    assert_eq!(high["relative_speed"], 1.0);
    assert_eq!(low["relative_speed"], 2.5);
    let d = descriptor(&json, "custom.qps");
    assert_eq!(d["unit"], "ops/s");
    assert_eq!(d["direction"], "higher_is_better");
}

#[test]
fn higher_is_better_terminal_output() {
    perfratio()
        .args(RUN)
        .assert()
        .success()
        .stdout(predicate::str::contains("(100.000 ops/s, −150.000 ops/s)"))
        .stdout(predicate::str::contains("2.50"))
        .stdout(predicate::str::contains("times higher value than"));
}

#[test]
fn default_direction_is_lower_without_declaration() {
    let json = json_of(&[
        "--runs=2",
        "--warmup=0",
        "--output-metric",
        "lat[ms]=lat=([0-9.]+)",
        "echo lat=5",
    ]);
    let d = descriptor(&json, "custom.lat");
    assert_eq!(d["unit"], "ms");
    assert_eq!(d["direction"], "lower_is_better");
}

#[test]
fn pareto_uses_the_declared_direction() {
    let json = json_of(&[
        "--runs=2",
        "--warmup=0",
        "--output-metric",
        "qps[higher]=qps=([0-9.]+)",
        "--pareto=custom:qps",
        "echo qps=100",
        "echo qps=250",
    ]);
    let text = json["pareto"].to_string();
    assert!(text.contains("higher_is_better"), "{text}");
}

#[test]
fn malformed_declaration_is_an_options_error() {
    perfratio()
        .args(["--output-metric", "x[ms,fast]=([0-9]+)", "echo 1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unit declared twice"));
    perfratio()
        .args(["--output-metric", "x[ms=([0-9]+)", "echo 1"])
        .assert()
        .failure();
}

#[test]
fn import_reads_the_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.json");
    perfratio()
        .args(RUN)
        .args(["--export-json", path.to_str().unwrap()])
        .assert()
        .success();

    let out = dir.path().join("re.json");
    perfratio()
        .args([
            "--metric",
            "custom:qps",
            "--import-json",
            path.to_str().unwrap(),
            "--export-json",
            out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("times higher value than"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let d = descriptor(&json, "custom.qps");
    assert_eq!(d["unit"], "ops/s");
    assert_eq!(d["direction"], "higher_is_better");
    assert_eq!(json["results"][1]["relative_speed"], 1.0);
}

#[test]
fn import_of_a_file_without_descriptors_defaults_to_lower() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.json");
    let mut json = json_of(&RUN);
    json["perfratio"].as_object_mut().unwrap().remove("metrics");
    std::fs::write(&path, json.to_string()).unwrap();
    perfratio()
        .args([
            "--import-json",
            path.to_str().unwrap(),
            "--metric",
            "custom:qps",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("times lower value than"));
}

#[test]
fn secondary_metric_shows_its_unit() {
    perfratio()
        .args([
            "--runs=2",
            "--warmup=0",
            "--output-metric",
            "lat[ms]=lat=([0-9.]+)",
            "echo lat=5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("lat [ms] (mean ± σ):"));
}
