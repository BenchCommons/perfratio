//! `--topdown` (#221): CLI parsing, terminal block, JSON and the delta table.
//! `--debug-mode` simulates deterministic slot counts (25% retiring, 12.5% bad
//! speculation, 12.5% frontend, 50% backend), so nothing here needs a PMU.

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
fn terminal_block_marks_the_primary_bottleneck_as_a_hypothesis() {
    perfratio()
        .args(["--debug-mode", "--runs=3", "--topdown", "sleep 0.5"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Topdown (level 1, Intel perf_metrics):",
        ))
        .stdout(predicate::str::contains("Retiring:"))
        .stdout(predicate::str::contains("25.0%"))
        .stdout(predicate::str::contains("Bad Speculation:"))
        .stdout(predicate::str::contains("Frontend Bound:"))
        .stdout(
            predicate::str::contains("Backend Bound:").and(predicate::str::contains(
                "50.0%   <- primary bottleneck (hypothesis)",
            )),
        )
        .stdout(predicate::str::contains("short run").not());
}

#[test]
fn not_printed_without_the_flag() {
    perfratio()
        .args(["--debug-mode", "--runs=2", "sleep 0.5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Topdown").not());
    let json = export(&["--runs=2", "sleep 0.5"]);
    assert_eq!(
        json["perfratio"]["measurements"]["topdown"],
        "not_requested"
    );
    assert!(json["results"][0].get("topdown").is_none());
    assert!(json["results"][0].get("topdown_summary").is_none());
}

#[test]
fn fractions_are_exported_per_run_with_a_summary() {
    let json = export(&["--runs=3", "--topdown", "sleep 0.5"]);
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["topdown"], "measured");
    assert!(m.get("topdown_unavailable_reason").is_none());
    let r = &json["results"][0];
    let t = &r["topdown"];
    assert_eq!(t["level"], 1);
    assert_eq!(t["source"], "intel_perf_metrics");
    for (key, value) in [
        ("retiring", 0.25),
        ("bad_speculation", 0.125),
        ("frontend_bound", 0.125),
        ("backend_bound", 0.5),
    ] {
        let runs = t[key].as_array().unwrap();
        assert_eq!(runs.len(), r["times"].as_array().unwrap().len(), "{key}");
        assert!(runs.iter().all(|v| v.as_f64() == Some(value)), "{key}");
        assert_eq!(r["topdown_summary"][key]["mean"], value, "{key}");
    }
    assert!(t.get("smt_contention").is_none());
    assert_eq!(r["topdown_summary"]["bottleneck"], "backend_bound");
    assert!(r.get("counters").is_none());
}

#[test]
fn coexists_with_counters_without_mixing_the_values() {
    let json = export(&["--runs=2", "--counters=cycles", "--topdown", "sleep 0.5"]);
    let r = &json["results"][0];
    let counters = r["counters"].as_object().unwrap();
    assert_eq!(counters.keys().collect::<Vec<_>>(), ["cycles"]);
    assert_eq!(r["counters_summary"]["cycles"]["mean"], 500_000_000.0);
    assert_eq!(r["topdown_summary"]["retiring"]["mean"], 0.25);
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["counters"], "measured");
    assert_eq!(m["topdown"], "measured");
}

#[test]
fn imported_results_keep_topdown() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("a.json");
    perfratio()
        .args(["--debug-mode", "--runs=2", "--topdown", "--export-json"])
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
        b["results"][0]["topdown"]["backend_bound"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        b["results"][0]["topdown_summary"]["bottleneck"],
        "backend_bound"
    );
}

#[test]
fn delta_table_has_a_row_per_class_per_class() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--topdown",
            "--delta-table",
            "sleep 0.1",
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Topdown: retiring"))
        .stdout(predicate::str::contains("Topdown: bad speculation"))
        .stdout(predicate::str::contains("Topdown: frontend bound"))
        .stdout(predicate::str::contains("Topdown: backend bound"));
}

/// Other platforms fail fast with the message of the spec.
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
#[test]
fn unsupported_platform_is_an_error() {
    perfratio()
        .args(["--runs=2", "--topdown", "--", env!("CARGO"), "--version"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "--topdown is only supported on Linux x86_64",
        ));
}

/// Real events: needs Intel Ice Lake or later, or AMD Zen 4 or later, and a
/// permissive `perf_event_paranoid`; only with `PERFRATIO_TEST_PERF=1`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn real_topdown_fractions_sum_to_one_on_a_pmu() {
    if std::env::var_os("PERFRATIO_TEST_PERF").is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("o.json");
    perfratio()
        .args(["--runs=5", "--topdown", "-N", "--export-json"])
        .arg(&out)
        .args(["--", "seq", "2000000"])
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(json["perfratio"]["measurements"]["topdown"], "measured");
    let s = &json["results"][0]["topdown_summary"];
    let total: f64 = [
        "retiring",
        "bad_speculation",
        "frontend_bound",
        "backend_bound",
    ]
    .iter()
    .map(|k| s[k]["mean"].as_f64().unwrap())
    .sum();
    assert!((0.9..=1.1).contains(&total), "{total}");
}
