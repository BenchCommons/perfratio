//! `--membw` (#378): CLI, terminal line, JSON and the degraded path.
//! `--debug-mode` simulates 2 GB/s read and 1 GB/s write with 80% of the resident bytes
//! on NUMA node 0, so nothing here needs an uncore PMU. The real counters need bare-metal
//! Linux and are checked in #337.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&json)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap()
}

#[test]
fn exports_bandwidth_numa_and_status() {
    let json = export(&["--runs=3", "--membw", "sleep 0.5"]);
    assert_eq!(json["schemaVersion"], "1.48");
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["membw"], "measured");
    assert_eq!(m["numa"], "measured");
    assert!(m.get("membw_unavailable_reason").is_none());
    let r = &json["results"][0];
    let runs = r["times"].as_array().unwrap().len();
    let membw = &r["membw"];
    assert_eq!(membw["source"], "mock");
    for key in ["read_bytes", "write_bytes", "bytes_per_second"] {
        assert_eq!(membw[key].as_array().unwrap().len(), runs, "{key}");
    }
    let rate = membw["bytes_per_second"][0].as_f64().unwrap();
    assert!((rate - 3.0e9).abs() / 3.0e9 < 1e-3, "{rate}");
    let summary = &r["membw_summary"];
    assert!((summary["bytes_per_second"]["mean"].as_f64().unwrap() - 3.0e9).abs() < 1e7);
    assert!(summary["read_bytes_per_second"]["mean"].as_f64().unwrap() > 1.9e9);
    let numa = &r["numa"];
    assert_eq!(numa["dominant_node"], 0);
    assert_eq!(numa["node_bytes"].as_array().unwrap().len(), runs);
    assert!((numa["mean_node_fraction"]["0"].as_f64().unwrap() - 0.8).abs() < 1e-9);
}

#[test]
fn concurrency_and_region_scope_report_why_there_is_no_reading() {
    for (flag, reason) in [
        (&["--concurrency=2"][..], "concurrency"),
        (&["--region", "--region-scope"][..], "region_scope"),
    ] {
        let mut args = vec!["--runs=2", "--membw"];
        args.extend(flag);
        args.push("sleep 0.1");
        let json = export(&args);
        let m = &json["perfratio"]["measurements"];
        assert_eq!(m["membw"], "unavailable", "{reason}");
        assert_eq!(m["membw_unavailable_reason"], reason, "{reason}");
        assert_eq!(m["numa"], "unavailable", "{reason}");
        assert_eq!(m["numa_unavailable_reason"], reason, "{reason}");
    }
}

#[test]
fn without_the_flag_nothing_is_added() {
    let json = export(&["--runs=2", "sleep 0.1"]);
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["membw"], "not_requested");
    assert_eq!(m["numa"], "not_requested");
    let r = &json["results"][0];
    for key in ["membw", "membw_summary", "numa"] {
        assert!(r.get(key).is_none(), "{key}");
    }
}

#[test]
fn prints_a_memory_line() {
    perfratio()
        .arg("--debug-mode")
        .args(["--runs=2", "--membw", "sleep 0.5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Memory:"))
        .stdout(predicate::str::contains(
            "3.0 GB/s (read 2.0 GB/s, write 1.0 GB/s)",
        ))
        .stdout(predicate::str::contains("NUMA node0 80%, node1 20%"));
}

/// Without `--debug-mode` the machine decides: where there is no uncore PMU (macOS, a CI
/// runner, a VM) the run still succeeds, warns, and says why in the export.
#[test]
fn degrades_without_an_uncore_pmu() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--membw", "-N", "--export-json"])
        .arg(&json)
        .arg("true")
        .assert()
        .success();
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let m = &doc["perfratio"]["measurements"];
    match m["membw"].as_str().unwrap() {
        "measured" => assert!(doc["results"][0].get("membw").is_some()),
        "unavailable" => {
            assert!(m["membw_unavailable_reason"].is_string());
            assert!(doc["results"][0].get("membw").is_none());
        }
        other => panic!("unexpected status {other}"),
    }
    assert!(["measured", "unavailable"].contains(&m["numa"].as_str().unwrap()));
}

#[test]
fn the_flag_is_in_the_help() {
    perfratio()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--membw"));
}
