//! Tests for the run metadata, labels and relative speed in JSON/CSV exports.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export(args: &[&str]) -> (serde_json::Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    let csv = dir.path().join("out.csv");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&json)
        .arg("--export-csv")
        .arg(&csv)
        .assert()
        .success();
    (
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap(),
        std::fs::read_to_string(&csv).unwrap(),
    )
}

#[test]
fn json_contains_run_metadata_and_labels() {
    let (json, _) = export(&[
        "--label",
        "commit=abc123",
        "--label",
        "runner=ci",
        "sleep 0.1",
    ]);
    let meta = &json["perfratio"];
    assert!(json.get("joulex").is_none());

    assert_eq!(meta["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(meta["labels"]["commit"], "abc123");
    assert_eq!(meta["labels"]["runner"], "ci");
    assert_eq!(meta["system"]["os"], std::env::consts::OS);
    assert!(meta["started_at"].as_str().unwrap().ends_with('Z'));
    // the program path is reduced to its file name (no home directory leak)
    let program = meta["command_line"][0].as_str().unwrap();
    assert!(
        !program.contains('/') && !program.contains('\\'),
        "{program}"
    );
}

#[test]
fn json_contains_relative_speed_to_the_fastest_result() {
    let (json, _) = export(&["sleep 0.2", "sleep 0.1"]);
    let results = json["results"].as_array().unwrap();

    assert!((results[0]["relative_speed"].as_f64().unwrap() - 2.0).abs() < 1e-9);
    assert!((results[1]["relative_speed"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    // the baseline has no spread relative to itself
    assert!(results[1].get("relative_speed_stddev").is_none());
}

#[test]
fn csv_appends_relative_speed_and_label_columns() {
    let (_, csv) = export(&["--label", "commit=abc123", "sleep 0.2", "sleep 0.1"]);
    let mut lines = csv.lines();

    assert_eq!(
        lines.next().unwrap(),
        "command,mean,stddev,median,user,system,min,max,relative_speed,relative_speed_stddev,label_commit"
    );
    assert!(lines.next().unwrap().ends_with(",abc123"));
}

#[test]
fn new_json_exports_can_be_imported_again() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--debug-mode", "--label", "k=v", "--export-json"])
        .arg(&json)
        .arg("sleep 0.1")
        .assert()
        .success();

    perfratio()
        .arg("--import-json")
        .arg(&json)
        .assert()
        .success()
        .stdout(predicate::str::contains("sleep 0.1 (imported)"));
}

#[test]
fn rejects_invalid_labels() {
    for label in ["novalue", "bad key=1"] {
        perfratio()
            .args(["--debug-mode", "--label", label, "sleep 0.1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Invalid label"));
    }
    perfratio()
        .args([
            "--debug-mode",
            "--label",
            "a=1",
            "--label",
            "a=2",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Duplicate label key 'a'"));
}

#[test]
fn json_contains_percentiles_and_geometric_mean() {
    let (json, _) = export(&["--runs=5", "sleep 0.1"]);
    let result = &json["results"][0];

    for key in ["p05", "p25", "p75", "p95"] {
        let value = result["percentiles"][key].as_f64().unwrap();
        assert!((value - 0.1).abs() < 1e-9, "{key}: {value}");
    }
    assert!((result["geometric_mean"].as_f64().unwrap() - 0.1).abs() < 1e-9);
}

#[test]
fn json_percentiles_are_imported_back_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    perfratio()
        .args(["--debug-mode", "--runs=5", "--export-json"])
        .arg(&first)
        .arg("sleep 0.1")
        .assert()
        .success();
    perfratio()
        .arg("--import-json")
        .arg(&first)
        .arg("--export-json")
        .arg(&second)
        .assert()
        .success();

    let read = |p: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    assert_eq!(
        read(&first)["results"][0]["percentiles"],
        read(&second)["results"][0]["percentiles"]
    );
}

#[test]
fn json_starts_with_schema_version_and_has_no_joulex_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(["--debug-mode", "--runs=2", "--export-json"])
        .arg(&path)
        .arg("sleep 0.1")
        .assert()
        .success();
    let text = std::fs::read_to_string(&path).unwrap();
    // `schemaVersion` must be the first key of the document
    assert!(
        text.starts_with("{\n  \"schemaVersion\": \"1.9\","),
        "{text}"
    );
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(json.get("joulex").is_none());
    let keys: Vec<&String> = json.as_object().unwrap().keys().collect();
    assert!(keys.contains(&&"perfratio".to_string()));
}

#[test]
fn measurements_without_energy_flag() {
    let (json, _) = export(&["--runs=1", "sleep 0.1"]);
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["time"], "measured");
    assert_eq!(m["memory"], "measured");
    assert_eq!(m["energy"], "not_requested");
    assert_eq!(m["resources"], "not_requested");
    assert!(m.get("energy_unavailable_reason").is_none());
}

#[test]
fn measurements_with_resource_usage_flag() {
    let (json, _) = export(&["--runs=1", "--resource-usage", "sleep 0.1"]);
    assert_eq!(json["perfratio"]["measurements"]["resources"], "measured");
}

#[test]
fn measurements_energy_requested_is_measured_or_unavailable_with_reason() {
    let (json, _) = export(&["--runs=1", "--energy", "sleep 0.1"]);
    let m = &json["perfratio"]["measurements"];
    match m["energy"].as_str().unwrap() {
        "measured" => assert!(m.get("energy_unavailable_reason").is_none()),
        "unavailable" => {
            let reason = m["energy_unavailable_reason"].as_str().unwrap();
            assert!(
                [
                    "unsupported_platform",
                    "rapl_not_found",
                    "rapl_unreadable",
                    "unsupported_cpu",
                    "ioreport_not_found",
                    "ioreport_frozen",
                    "emi_not_found",
                    "emi_unreadable",
                ]
                .contains(&reason),
                "{reason}"
            );
            if cfg!(target_os = "macos") {
                assert!(
                    ["unsupported_cpu", "ioreport_not_found", "ioreport_frozen"].contains(&reason),
                    "{reason}"
                );
            }
            if cfg!(windows) {
                assert!(
                    ["emi_not_found", "emi_unreadable"].contains(&reason),
                    "{reason}"
                );
            }
        }
        other => panic!("unexpected energy status {other}"),
    }
}

/// Snapshot of the stable part of the 1.0 contract: key layout of the document,
/// the metadata object and the measurements block (values of the host and time
/// are masked).
#[test]
fn schema_1_1_layout_snapshot() {
    let (json, _) = export(&["--runs=2", "--label", "lang=rust", "sleep 0.1"]);
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    let mut layout = serde_json::Map::new();
    layout.insert("top_level".into(), keys(&json).into());
    layout.insert("perfratio".into(), keys(&json["perfratio"]).into());
    layout.insert(
        "measurements".into(),
        json["perfratio"]["measurements"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.as_str() != "energy_unavailable_reason")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect::<serde_json::Map<_, _>>()
            .into(),
    );
    let mut result_keys = keys(&json["results"][0]);
    result_keys.sort();
    layout.insert("result_guaranteed".into(), {
        let guaranteed = [
            "command",
            "mean",
            "stddev",
            "median",
            "user",
            "system",
            "min",
            "max",
            "times",
            "user_times",
            "system_times",
            "exit_codes",
            "memory_usage_byte",
        ];
        for k in guaranteed {
            assert!(result_keys.iter().any(|r| r == k), "missing {k}");
        }
        guaranteed
            .iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
            .into()
    });
    insta::assert_yaml_snapshot!(serde_json::Value::Object(layout), @r#"
    measurements:
      counters: not_requested
      energy: not_requested
      memory: measured
      resources: not_requested
      time: measured
    perfratio:
      - command_line
      - labels
      - measurements
      - started_at
      - system
      - version
    result_guaranteed:
      - command
      - mean
      - stddev
      - median
      - user
      - system
      - min
      - max
      - times
      - user_times
      - system_times
      - exit_codes
      - memory_usage_byte
    top_level:
      - metric
      - perfratio
      - results
      - schemaVersion
    "#);
}

#[test]
fn import_json_reads_legacy_files_with_only_joulex_key() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("legacy.json");
    std::fs::write(
        &legacy,
        r#"{"joulex": {"version": "0.1.0", "command_line": [], "started_at": "2024-01-01T00:00:00Z", "system": {"os": "linux", "arch": "x86_64", "cpus": 1}},
"results": [{"command": "sleep 0.1", "mean": 0.1, "stddev": null, "median": 0.1, "user": 0.0, "system": 0.0, "min": 0.1, "max": 0.1, "times": [0.1], "exit_codes": [0]}]}"#,
    )
    .unwrap();
    let out = dir.path().join("out.json");
    perfratio()
        .arg("--import-json")
        .arg(&legacy)
        .arg("--export-json")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicates::str::contains("sleep 0.1"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let results = json["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["command"], "sleep 0.1");
    assert_eq!(results[0]["mean"].as_f64().unwrap(), 0.1);
    assert_eq!(results[0]["times"].as_array().unwrap().len(), 1);
    assert!(json.get("joulex").is_none());
}
