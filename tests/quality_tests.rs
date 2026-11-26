//! Tests for the measurement quality report, `--quality` and `--fail-on-quality`
//! (#318, `docs/specs/measurement-quality.md`). Deterministic through `--debug-mode`.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Twelve simulated runs of 0.1 s with 0.9 s outliers: an unreliable measurement
fn noisy() -> Vec<&'static str> {
    vec![
        "--runs=1",
        "--aggregate-parameter-runs",
        "-L",
        "t",
        "0.1,0.1,0.9,0.1,0.1,0.9,0.1,0.1,0.1,0.9,0.1,0.1",
        "sleep {t}",
    ]
}

fn check<'a>(
    result: &'a serde_json::Value,
    id: &str,
    metric: Option<&str>,
) -> &'a serde_json::Value {
    result["quality"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id && c.get("metric").and_then(|m| m.as_str()) == metric)
        .unwrap_or_else(|| panic!("no check {id}"))
}

#[test]
fn every_benchmark_has_a_report_in_the_json() {
    let json = export(&["--runs=30", "sleep 0.1", "sleep 0.2"]);
    for result in json["results"].as_array().unwrap() {
        let quality = &result["quality"];
        assert_eq!(quality["grade"], "comparable", "{quality}");
        assert_eq!(
            check(result, "sample_sufficiency", Some("wall"))["status"],
            "pass"
        );
        assert_eq!(
            check(result, "workload_validation", None)["status"],
            "not_applicable"
        );
        assert_eq!(
            check(result, "counter_coverage", Some("counters"))["status"],
            "not_applicable"
        );
    }
}

#[test]
fn few_runs_are_flagged() {
    let json = export(&["--runs=3", "sleep 0.1"]);
    let result = &json["results"][0];
    assert_eq!(result["quality"]["grade"], "with_caveats");
    assert_eq!(
        check(result, "sample_sufficiency", Some("wall"))["status"],
        "warn"
    );
    assert_eq!(
        check(result, "trend", Some("time"))["status"],
        "unavailable"
    );
}

#[test]
fn a_noisy_fixture_flips_the_grade() {
    let json = export(&noisy());
    let result = &json["results"][0];
    assert_eq!(result["quality"]["grade"], "unreliable");
    assert_eq!(
        check(result, "statistical_precision", Some("wall"))["status"],
        "fail"
    );
    assert_eq!(
        check(result, "outlier_burden", Some("time"))["status"],
        "warn"
    );
}

#[test]
fn quality_modes_control_the_terminal_only() {
    // default: printed because something is not 'pass'
    perfratio()
        .args(["--debug-mode", "--runs=3", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Measurement quality: with_caveats",
        ));
    // a clean result prints nothing by default
    perfratio()
        .args(["--debug-mode", "--runs=30", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Measurement quality").not());
    // full lists the passing checks too
    perfratio()
        .args(["--debug-mode", "--runs=30", "--quality=full", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Measurement quality: comparable"))
        .stdout(predicate::str::contains(
            "pass           sample_sufficiency",
        ));
    // off prints nothing, the JSON keeps the report
    perfratio()
        .args(["--debug-mode", "--runs=3", "--quality=off", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Measurement quality").not());
    let json = export(&["--runs=3", "--quality=off", "sleep 0.1"]);
    assert!(json["results"][0]["quality"]["checks"].is_array());
}

#[test]
fn fail_on_quality_is_a_ci_gate() {
    // 3 runs: with_caveats
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--fail-on-quality=comparable",
            "sleep 0.1",
        ])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("worse than 'comparable'"));
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--fail-on-quality=with_caveats",
            "sleep 0.1",
        ])
        .assert()
        .success();
    // the noisy fixture is unreliable
    perfratio()
        .args(["--debug-mode", "--fail-on-quality=with_caveats"])
        .args(noisy())
        .assert()
        .code(5);
    perfratio()
        .args(["--debug-mode", "--fail-on-quality=unreliable"])
        .args(noisy())
        .assert()
        .success();
    // a clean result passes the strictest gate
    perfratio()
        .args([
            "--debug-mode",
            "--runs=30",
            "--fail-on-quality=comparable",
            "sleep 0.1",
        ])
        .assert()
        .success();
}

#[test]
fn a_wrong_result_makes_the_measurement_unreliable() {
    let json = export(&[
        "--runs=30",
        "--on-invalid=keep",
        "--expect-output-sha256",
        &"0".repeat(64),
        "sleep 0.1 0 0 0 hello",
    ]);
    let result = &json["results"][0];
    assert_eq!(check(result, "workload_validation", None)["status"], "fail");
    assert_eq!(result["quality"]["grade"], "unreliable");
}

#[test]
fn energy_coverage_with_a_sampler_passes() {
    let json = export(&["--runs=12", "--energy", "sleep 0.1"]);
    let energy = check(&json["results"][0], "energy_coverage", Some("energy"));
    assert_ne!(energy["status"], "not_applicable");
    // the mock sampler gives every run a sample
    assert_eq!(energy["status"], "pass");
}

#[test]
fn invalid_grade_is_rejected() {
    perfratio()
        .args(["--debug-mode", "--fail-on-quality=great", "sleep 0.1"])
        .assert()
        .failure();
}
