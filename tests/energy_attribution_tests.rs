//! `--energy-attribution` (#381): debug mode simulates the host (every snapshot adds 1 s of busy
//! CPU time and 0.5 s of cgroup time) and the energy sampler (1.25 J per run), so these run
//! everywhere. `sleep 0.1 0.2 0.05` simulates 0.2 s user + 0.05 s system CPU time.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export(args: &[&str]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    perfratio()
        .args(["--debug-mode", "--runs=3", "--energy"])
        .args(args)
        .arg("--export-json")
        .arg(&out)
        .arg("sleep 0.1 0.2 0.05")
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap()
}

#[test]
fn cpu_time_share_is_exported_as_an_estimate_next_to_the_measurement() {
    let json = export(&["--energy-attribution", "cpu-time"]);
    let r = &json["results"][0];
    let a = &r["energy_attribution"];
    assert_eq!(a["kind"], "estimated");
    assert_eq!(a["model"], "cpu-time-share");
    assert_eq!(a["scope"], "process");
    assert_eq!(a["samples"], 3);
    assert_eq!(a["clamped_runs"], 0);
    // 0.25 s of CPU over 1 s of host busy time
    assert!((a["mean_share"].as_f64().unwrap() - 0.25).abs() < 1e-9);
    assert!((a["mean_joules"].as_f64().unwrap() - 1.25 * 0.25).abs() < 1e-9);
    assert_eq!(a["joules"].as_array().unwrap().len(), 3);
    // the measurement is untouched
    assert_eq!(r["mean_energy_joules"], 1.25);
    assert_eq!(r["energy_joules"][0], 1.25);
}

#[test]
fn cgroup_share_uses_the_container_cpu_time() {
    let json = export(&["--energy-attribution", "cgroup"]);
    let a = &json["results"][0]["energy_attribution"];
    assert_eq!(a["model"], "cgroup-cpu-share");
    assert_eq!(a["scope"], "cgroup");
    assert!((a["mean_share"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((a["mean_joules"].as_f64().unwrap() - 0.625).abs() < 1e-9);
}

#[test]
fn the_estimate_is_listed_as_an_estimated_metric_descriptor() {
    let json = export(&["--energy-attribution", "cpu-time"]);
    let metrics = json["perfratio"]["metrics"].as_array().unwrap();
    let d = metrics
        .iter()
        .find(|m| m["id"] == "energy.attributed")
        .expect("descriptor");
    assert_eq!(d["kind"], "estimated");
    assert_eq!(d["unit"], "J");
}

#[test]
fn nothing_is_added_without_the_flag() {
    let json = export(&[]);
    assert!(json["results"][0].get("energy_attribution").is_none());
    let metrics = json["perfratio"]["metrics"].as_array().unwrap();
    assert!(metrics.iter().all(|m| m["id"] != "energy.attributed"));
}

#[test]
fn the_console_labels_the_value_as_estimated() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--energy",
            "--energy-attribution",
            "cpu-time",
            "sleep 0.1 0.2 0.05",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Energy (estimated):"))
        .stdout(predicate::str::contains("cpu-time-share"));
}

#[test]
fn attribution_needs_energy() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--energy-attribution",
            "cpu-time",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--energy"));
}

#[test]
fn an_unknown_model_is_rejected() {
    perfratio()
        .args([
            "--debug-mode",
            "--energy",
            "--energy-attribution",
            "cycles",
            "sleep 0.1",
        ])
        .assert()
        .failure();
}

/// Where /proc/stat is missing the run still succeeds, says why there is no estimate and
/// exports none.
#[test]
#[cfg(not(target_os = "linux"))]
fn without_proc_stat_the_estimate_degrades_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("r.json");
    perfratio()
        .args([
            "--runs=2",
            "--energy",
            "--energy-attribution",
            "cpu-time",
            "--export-json",
        ])
        .arg(&out)
        .arg("true")
        .assert()
        .success()
        .stderr(predicate::str::contains("--energy-attribution"))
        .stderr(predicate::str::contains("unavailable"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out).unwrap()).unwrap();
    assert!(json["results"][0].get("energy_attribution").is_none());
}
