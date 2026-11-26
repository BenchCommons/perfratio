//! `--counters` (#49): CLI parsing, JSON and the unavailable path. `--debug-mode`
//! simulates deterministic counter values, so nothing here needs a PMU.

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
fn default_list_is_exported_per_run_with_summary_and_rates() {
    let json = export(&["--runs=3", "--counters", "sleep 0.5"]);
    assert_eq!(json["perfratio"]["measurements"]["counters"], "measured");
    let r = &json["results"][0];
    assert_eq!(r["counters_scope"], "all");
    let instr = r["counters"]["instructions"].as_array().unwrap();
    assert_eq!(instr.len(), r["times"].as_array().unwrap().len());
    assert_eq!(instr[0], 1_000_000_000u64);
    assert_eq!(r["counters"].as_object().unwrap().len(), 6);
    assert_eq!(r["counters_summary"]["cycles"]["mean"], 500_000_000.0);
    assert_eq!(r["efficiency"]["ipc"], 2.0);
    assert!(r.get("counters_multiplexed_runs").is_none());
}

#[test]
fn explicit_list_and_terminal_line() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--counters=cycles,instructions,cycles",
        ])
        .arg("sleep 0.1")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Counters (mean):    cycles 100.0M (IPC 2.00) · instr 200.0M",
        ));
    let json = export(&["--runs=2", "--counters=cycles", "sleep 0.1"]);
    let counters = json["results"][0]["counters"].as_object().unwrap();
    assert_eq!(counters.keys().collect::<Vec<_>>(), ["cycles"]);
}

#[test]
fn without_the_flag_nothing_is_added() {
    let json = export(&["--runs=2", "sleep 0.1"]);
    assert_eq!(
        json["perfratio"]["measurements"]["counters"],
        "not_requested"
    );
    assert!(json["results"][0].get("counters").is_none());
    assert!(json["results"][0].get("counters_scope").is_none());
}

#[test]
fn unknown_counter_lists_the_valid_names() {
    perfratio()
        .args(["--debug-mode", "--counters=instructions,bogus", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown counter 'bogus'"))
        .stderr(predicate::str::contains("branch-misses"));
}

#[test]
fn bare_flag_does_not_swallow_the_command() {
    perfratio()
        .args(["--debug-mode", "--runs=1", "--counters", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Benchmark 1: sleep 0.1"));
}

#[test]
fn imported_results_keep_counters() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("a.json");
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--counters=cycles",
            "--export-json",
        ])
        .arg(&json)
        .arg("sleep 0.1")
        .assert()
        .success();
    let out = dir.path().join("b.json");
    perfratio()
        .arg("--import-json")
        .arg(&json)
        .arg("--export-json")
        .arg(&out)
        .assert()
        .success();
    let b: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        b["results"][0]["counters"]["cycles"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_platform_warns_once_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--counters", "--export-json"])
        .arg(&json)
        .args(["--", env!("CARGO"), "--version"])
        .assert()
        .success()
        .stderr(predicate::str::contains("only supported on Linux").count(1));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let m = &v["perfratio"]["measurements"];
    assert_eq!(m["counters"], "unavailable");
    assert_eq!(m["counters_unavailable_reason"], "unsupported_platform");
    assert!(v["results"][0].get("counters").is_none());
}

/// Real counters: needs a PMU, so only with `PERFRATIO_TEST_PERF=1`.
#[cfg(target_os = "linux")]
#[test]
fn real_instructions_are_counted() {
    if std::env::var_os("PERFRATIO_TEST_PERF").is_none() {
        return;
    }
    let json = {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("o.json");
        perfratio()
            .args(["--runs=5", "--counters=instructions", "-N", "--export-json"])
            .arg(&out)
            .args(["--", "seq", "100000"])
            .assert()
            .success();
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&out).unwrap()).unwrap()
    };
    assert_eq!(json["perfratio"]["measurements"]["counters"], "measured");
    assert!(
        json["results"][0]["counters_summary"]["instructions"]["mean"]
            .as_f64()
            .unwrap()
            > 1e6
    );
}

#[test]
fn metric_instructions_ranks_by_the_counter_and_enables_it() {
    // no `--counters`: `--metric` collects its own counter
    let json = export(&[
        "--runs=3",
        "--metric=instructions",
        "sleep 0.5",
        "sleep 0.2",
    ]);
    assert_eq!(json["perfratio"]["measurements"]["counters"], "measured");
    let r = &json["results"][0];
    assert_eq!(r["counters"].as_object().unwrap().len(), 1);
    assert_eq!(r["counters_summary"]["instructions"]["mean"], 1e9);
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--metric=instructions",
            "sleep 0.5",
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Instructions (mean ± σ):"))
        .stdout(predicate::str::contains("times fewer instructions than"));
}

#[test]
fn metric_cycles_keeps_an_explicit_counter_list() {
    let json = export(&[
        "--runs=2",
        "--metric=cycles",
        "--counters=instructions",
        "sleep 0.1",
    ]);
    let counters = json["results"][0]["counters"].as_object().unwrap();
    assert_eq!(
        counters.keys().collect::<Vec<_>>(),
        ["cycles", "instructions"]
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn metric_instructions_fails_clearly_without_counters() {
    perfratio()
        .args(["--runs=2", "--metric=instructions", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'--metric instructions' requires a 'instructions' reading for every run",
        ));
}
