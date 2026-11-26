//! Tests for --concurrency (#53). `--debug-mode` simulates the instances, so the
//! assertions are deterministic; one real run is Unix-only (it needs `sleep`).

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::Value;

fn export(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json
}

#[test]
fn invalid_values_are_rejected() {
    for invalid in ["0", "abc", "-1", "1.5", ""] {
        perfratio()
            .args([
                "--debug-mode",
                &format!("--concurrency={invalid}"),
                "sleep 0.1",
            ])
            .assert()
            .failure()
            .stderr(predicate::str::contains("--concurrency"));
    }
}

#[test]
fn incompatible_options_are_rejected() {
    for other in [
        &["--until", "x"][..],
        &["--show-output-on-failure"],
        &["--output-metric", "a=([0-9]+)"],
        &["--subtract", "sleep 0.01"],
    ] {
        perfratio()
            .args(["--debug-mode", "--concurrency", "2"])
            .args(other)
            .arg("sleep 0.1")
            .assert()
            .failure();
    }
}

#[test]
fn concurrency_one_is_identical_to_the_default() {
    let plain = export(&["--debug-mode", "--runs=3", "sleep 0.2"]);
    let one = export(&[
        "--debug-mode",
        "--runs=3",
        "--concurrency",
        "1",
        "sleep 0.2",
    ]);
    let (a, b) = (&plain["results"][0], &one["results"][0]);
    assert_eq!(a["times"], b["times"]);
    assert_eq!(a["exit_codes"], b["exit_codes"]);
    for key in [
        "concurrency",
        "instance_times",
        "throughput",
        "instance_percentiles",
    ] {
        assert!(b.get(key).is_none(), "{key} must be absent for N = 1");
    }
}

#[test]
fn json_has_instance_fields_for_both_schedules() {
    for schedule in ["grouped", "round-robin"] {
        let json = export(&[
            "--debug-mode",
            "--runs=3",
            "--concurrency",
            "4",
            "--schedule",
            schedule,
            "sleep 0.2",
            "sleep 0.1",
        ]);
        assert_eq!(json["schemaVersion"], "1.4");
        for result in json["results"].as_array().unwrap() {
            assert_eq!(result["concurrency"], 4);
            let times = result["times"].as_array().unwrap();
            let rows = result["instance_times"].as_array().unwrap();
            assert_eq!(rows.len(), 3);
            assert_eq!(result["exit_codes"].as_array().unwrap().len(), 12);
            for (makespan, row) in times.iter().zip(rows) {
                let row = row.as_array().unwrap();
                assert_eq!(row.len(), 4);
                let slowest = row.iter().map(|v| v.as_f64().unwrap()).fold(0.0, f64::max);
                assert_eq!(makespan.as_f64().unwrap(), slowest);
            }
            let p = &result["instance_percentiles"];
            assert!(p["p50"].as_f64().unwrap() <= p["p95"].as_f64().unwrap());
            assert!(p["p95"].as_f64().unwrap() <= p["max"].as_f64().unwrap());
            let throughput = result["throughput"].as_f64().unwrap();
            let mean = result["mean"].as_f64().unwrap();
            assert!((throughput - 4.0 / mean).abs() < 1e-9);
        }
    }
}

#[test]
fn makespan_is_not_the_sum_and_cpu_time_is_summed() {
    let json = export(&[
        "--debug-mode",
        "--runs=2",
        "--concurrency",
        "4",
        "sleep 0.2 0.1 0.05 1000",
    ]);
    let r = &json["results"][0];
    // 4 simulated instances of 0.2 s overlap: about 0.2 s, not 0.8 s
    let mean = r["mean"].as_f64().unwrap();
    assert!((0.2..0.25).contains(&mean), "mean {mean}");
    // user and system are summed over the 4 instances, memory is the maximum
    assert!((r["user"].as_f64().unwrap() - 0.4).abs() < 1e-9);
    assert!((r["system"].as_f64().unwrap() - 0.2).abs() < 1e-9);
    assert_eq!(r["memory_usage_byte"][0], 1000);
}

#[test]
fn terminal_output_shows_instance_statistics() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--concurrency",
            "2",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Concurrency:"))
        .stdout(predicate::str::contains("Per-instance (mean):"))
        .stdout(predicate::str::contains("Throughput:"));
}

#[test]
fn exported_json_can_be_imported_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--concurrency",
            "3",
            "sleep 0.1",
        ])
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    perfratio()
        .arg("--import-json")
        .arg(&path)
        .arg("--export-json")
        .arg(dir.path().join("again.json"))
        .assert()
        .success();
}

#[cfg(unix)]
#[test]
fn real_instances_run_simultaneously() {
    let json = export(&[
        "--concurrency",
        "4",
        "-N",
        "--runs=3",
        "--style=basic",
        "sleep 0.2",
    ]);
    let r = &json["results"][0];
    let mean = r["mean"].as_f64().unwrap();
    // 4 x 0.2 s sequentially would be 0.8 s
    assert!((0.19..0.6).contains(&mean), "mean {mean}");
    assert_eq!(r["instance_times"].as_array().unwrap().len(), 3);
    assert_eq!(r["instance_times"][0].as_array().unwrap().len(), 4);
    assert_eq!(r["exit_codes"].as_array().unwrap().len(), 12);
}

#[cfg(unix)]
#[test]
fn a_failing_instance_fails_the_run_unless_ignored() {
    perfratio()
        .args([
            "--concurrency",
            "3",
            "-N",
            "--runs=2",
            "--style=basic",
            "false",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-zero exit code"));
    let json = export(&[
        "--concurrency",
        "3",
        "-N",
        "--runs=2",
        "-i",
        "--style=basic",
        "false",
    ]);
    let codes = json["results"][0]["exit_codes"].as_array().unwrap();
    assert_eq!(codes.len(), 6);
    assert!(codes.iter().all(|c| c == 1));
}

#[cfg(unix)]
#[test]
fn timeout_kills_all_instances() {
    let start = std::time::Instant::now();
    perfratio()
        .args([
            "--concurrency",
            "3",
            "--timeout",
            "150ms",
            "-N",
            "--runs=2",
            "--style=basic",
            "sleep 5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Timed out"));
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
}
