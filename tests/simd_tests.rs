//! `--simd` (#211): CLI, terminal block, JSON and the unavailable path.
//! `--debug-mode` simulates a vectorized kernel (1.2M scalar, 0.1M 128-bit,
//! 32.4M 256-bit retired FP instructions per second), so nothing here needs a PMU.

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
fn simd_alone_exports_series_summary_and_status() {
    let json = export(&["--runs=3", "--simd", "sleep 0.5"]);
    assert_eq!(json["schemaVersion"], "1.45");
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["simd"], "measured");
    assert_eq!(m["counters"], "not_requested");
    assert!(m.get("simd_unavailable_reason").is_none());
    let r = &json["results"][0];
    let simd = &r["simd"];
    assert_eq!(simd["unit"], "instructions");
    assert_eq!(simd["arch"], "x86_64");
    let times = r["times"].as_array().unwrap().len();
    for key in ["scalar_fp", "simd_128", "simd_256", "simd_512"] {
        assert_eq!(simd[key].as_array().unwrap().len(), times, "{key}");
    }
    assert_eq!(simd["scalar_fp"][0], 600_000);
    assert_eq!(simd["simd_256"][0], 16_200_000);
    let ratio = simd["vectorization_ratio"][0].as_f64().unwrap();
    assert!((ratio - 0.9644).abs() < 1e-3, "{ratio}");
    assert_eq!(r["simd_summary"]["scalar_fp"]["mean"], 600_000.0);
    assert!(
        r["simd_summary"]["vectorization_ratio"]["mean"]
            .as_f64()
            .unwrap()
            > 0.96
    );
    assert_eq!(r["simd_scope"], "all");
    // `--simd` alone adds no basic counters
    assert!(r.get("counters").is_none());
    assert!(r.get("counters_scope").is_none());
    assert!(r.get("simd_scaling").is_none());
}

#[test]
fn simd_with_counters_keeps_both_groups_apart() {
    let json = export(&["--runs=2", "--counters=cycles", "--simd", "sleep 0.5"]);
    let m = &json["perfratio"]["measurements"];
    assert_eq!(
        (m["counters"].as_str(), m["simd"].as_str()),
        (Some("measured"), Some("measured"))
    );
    let r = &json["results"][0];
    let counters = r["counters"].as_object().unwrap();
    assert_eq!(counters.keys().collect::<Vec<_>>(), ["cycles"]);
    assert_eq!(counters["cycles"][0], 500_000_000u64);
    assert_eq!(r["simd"]["scalar_fp"][0], 600_000);
}

#[test]
fn terminal_block_shows_buckets_and_ratio() {
    perfratio()
        .args(["--debug-mode", "--runs=2", "--simd", "sleep 0.5"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "SIMD / FP breakdown (x86_64, instructions):",
        ))
        .stdout(predicate::str::contains("Scalar FP:"))
        .stdout(predicate::str::contains("256-bit SIMD:"))
        .stdout(predicate::str::contains("Vectorization ratio:   96.4%"));
}

#[test]
fn without_the_flag_nothing_is_added() {
    let json = export(&["--runs=2", "sleep 0.1"]);
    assert_eq!(json["perfratio"]["measurements"]["simd"], "not_requested");
    let r = &json["results"][0];
    for key in ["simd", "simd_summary", "simd_scaling", "simd_scope"] {
        assert!(r.get(key).is_none(), "{key}");
    }
}

#[test]
fn imported_results_keep_simd() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("a.json");
    perfratio()
        .args(["--debug-mode", "--runs=2", "--simd", "--export-json"])
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
    let read = |p: &std::path::Path| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    };
    let (a, b) = (read(&json), read(&out));
    assert_eq!(a["results"][0]["simd"], b["results"][0]["simd"]);
    assert_eq!(
        a["results"][0]["simd_summary"],
        b["results"][0]["simd_summary"]
    );
}

#[test]
fn delta_table_has_a_ratio_row_without_verdict() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--simd",
            "--delta-table",
            "sleep 0.1",
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Vectorization ratio"));
}

/// Real hardware path: whatever the machine is, `--simd` must not abort. It
/// measures on a supported Linux CPU with a PMU and is `unavailable` with a
/// reason everywhere else (macOS, Windows, VMs, hybrid or unknown CPUs).
#[test]
fn real_run_never_aborts_and_reports_a_status() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--simd", "--export-json"])
        .arg(&json)
        .args(["--", env!("CARGO"), "--version"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let m = &v["perfratio"]["measurements"];
    match m["simd"].as_str().unwrap() {
        "measured" => assert!(v["results"][0]["simd"]["scalar_fp"].is_array()),
        "unavailable" => {
            let reason = m["simd_unavailable_reason"].as_str().unwrap();
            assert!(
                [
                    "unsupported_platform",
                    "unsupported_cpu",
                    "perf_paranoid",
                    "no_pmu",
                    "hybrid_cpu_unpinned",
                    "other"
                ]
                .contains(&reason),
                "{reason}"
            );
            assert!(v["results"][0].get("simd").is_none());
        }
        other => panic!("unexpected status {other}"),
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_platform_warns_once_and_continues() {
    perfratio()
        .args(["--runs=2", "--simd", "--", env!("CARGO"), "--version"])
        .assert()
        .success()
        .stderr(predicate::str::contains("'--simd' is only supported on Linux").count(1));
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--simd", "--export-json"])
        .arg(&json)
        .args(["--", env!("CARGO"), "--version"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["perfratio"]["measurements"]["simd"], "unavailable");
    assert_eq!(
        v["perfratio"]["measurements"]["simd_unavailable_reason"],
        "unsupported_platform"
    );
}

#[test]
fn scope_applies_to_simd_and_split_is_rejected() {
    let json = export(&["--runs=2", "--simd", "--scope=user", "sleep 0.1"]);
    assert_eq!(json["results"][0]["simd_scope"], "user");
    perfratio()
        .args(["--debug-mode", "--simd", "--scope=split", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'--simd' cannot be combined with '--scope split'",
        ));
}

#[test]
fn simd_works_next_to_counters_and_topdown() {
    let json = export(&[
        "--runs=2",
        "--counters=cycles",
        "--simd",
        "--topdown",
        "sleep 0.5",
    ]);
    let r = &json["results"][0];
    assert_eq!(r["counters"]["cycles"][0], 500_000_000u64);
    assert_eq!(r["simd"]["scalar_fp"][0], 600_000);
    assert!(r.get("topdown").is_some(), "topdown series is kept");
}
