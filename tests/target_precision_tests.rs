//! Tests for --target-precision / --max-benchmarking-time.

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::Value;

fn result(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--style=basic", "--export-json"])
        .arg(&json)
        .args(args)
        .assert()
        .success();
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    json["results"][0].clone()
}

fn run_count(args: &[&str]) -> usize {
    result(args)["times"].as_array().unwrap().len()
}

#[test]
fn the_json_export_records_the_precision() {
    let precision = &result(&["--debug-mode", "--target-precision=1%", "sleep 0.1"])["precision"];
    assert_eq!(precision["target"], 0.01);
    assert_eq!(precision["reached"], 0.0);
    assert_eq!(precision["confidence"], 0.95);
    assert_eq!(precision["met"], true);

    // Only with --target-precision
    assert!(result(&["--debug-mode", "--runs=3", "sleep 0.1"])
        .get("precision")
        .is_none());
}

#[test]
fn a_precise_command_stops_at_the_minimum_number_of_runs() {
    // The mock's times are constant: the CI is 0 as soon as there are 2 runs
    assert_eq!(
        run_count(&["--debug-mode", "--target-precision=1%", "sleep 0.1"]),
        10
    );
    assert_eq!(
        run_count(&[
            "--debug-mode",
            "--target-precision=1%",
            "--min-runs=3",
            "sleep 0.1"
        ]),
        3
    );
}

#[test]
fn the_precision_reached_is_reported() {
    perfratio()
        .args(["--debug-mode", "--target-precision=1%", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("10 runs (±0.0% @95%, target 1%)"));
}

#[test]
fn aggregated_runs_stop_only_after_whole_cycles() {
    let runs = run_count(&[
        "--debug-mode",
        "--target-precision=1%",
        "--min-runs=4",
        "--aggregate-parameter-runs",
        "-L",
        "t",
        "1,2,3",
        "sleep 0.{t}",
    ]);
    assert_eq!(runs % 3, 0, "{runs}");
    assert!(runs >= 4);
}

#[test]
fn invalid_values_are_rejected() {
    for value in ["0%", "-1", "fast"] {
        perfratio()
            .arg(format!("--target-precision={value}"))
            .arg("echo")
            .assert()
            .failure()
            .stderr(predicate::str::contains("Invalid '--target-precision'"));
    }
    perfratio()
        .args(["--target-precision=1%", "--runs=5", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
    perfratio()
        .args(["--max-benchmarking-time=5", "echo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--target-precision"));
}

#[cfg(unix)]
mod unix {
    use super::*;

    /// 10–50 ms at random: a 0.1% target can't be reached in a few runs.
    const NOISY: &str = "sleep 0.0$(( $(od -An -N1 -tu1 /dev/urandom) % 5 + 1 ))";

    #[test]
    fn max_runs_bounds_the_runs_and_warns() {
        perfratio()
            .args([
                "--target-precision=0.1%",
                "--max-runs=12",
                "--style=basic",
                NOISY,
            ])
            .assert()
            .success()
            .stdout(predicate::str::contains("12 runs (±"))
            .stderr(predicate::str::contains(
                "The target precision of ±0.1% was not reached after 12 runs",
            ));
    }

    #[test]
    fn an_unmet_target_is_recorded_in_the_json() {
        let result = result(&["--target-precision=0.1%", "--max-runs=8", NOISY]);
        let precision = &result["precision"];
        assert_eq!(precision["met"], false);
        assert!(precision["reached"].as_f64().unwrap() > 0.001);
    }

    #[test]
    fn the_time_budget_bounds_the_runs() {
        let started = std::time::Instant::now();
        perfratio()
            .args(["--target-precision=0.01%", "--max-benchmarking-time=1"])
            .args(["--style=basic", NOISY])
            .assert()
            .success()
            .stderr(predicate::str::contains("was not reached"));
        assert!(started.elapsed().as_secs_f64() < 15.0);
    }

    /// Alternates between 10 ms and 30 ms by run number, so the spread is
    /// fixed by construction and a 0.1% target can't be reached in 11 runs,
    /// however quiet the machine is.
    fn alternating(counter: &std::path::Path) -> String {
        let c = counter.display();
        format!(
            "n=$(cat {c} 2>/dev/null || echo 0); n=$((n + 1)); echo $n > {c}; \
             sleep 0.0$(( n % 2 * 2 + 1 ))"
        )
    }

    #[test]
    fn round_robin_stops_each_command_at_its_target() {
        // A constant `sleep 0.01` could reach ±0.1% after the minimum of 10
        // runs on a quiet machine and stop before 11 (#244).
        let dir = tempfile::tempdir().unwrap();
        let a = alternating(&dir.path().join("a"));
        let b = alternating(&dir.path().join("b"));
        perfratio()
            .args([
                "--target-precision=0.1%",
                "--max-runs=11",
                "--schedule=round-robin",
            ])
            .args(["--style=basic", &a, &b])
            .assert()
            .success()
            .stdout(predicate::str::contains("11 runs (±").count(2));
    }
}

#[test]
fn the_json_export_names_the_stopping_rule() {
    let fixed = &result(&["--debug-mode", "--target-precision=1%", "sleep 0.1"])["precision"];
    assert_eq!(fixed["stopping_rule"], "fixed_ci");
    assert_eq!(fixed["achieved_half_width"], 0.0);

    let sequential = &result(&[
        "--debug-mode",
        "--target-precision=1%",
        "--target-precision-method=sequential",
        "sleep 0.1",
    ])["precision"];
    assert_eq!(sequential["stopping_rule"], "confidence_sequence");
    assert_eq!(sequential["achieved_half_width"], 0.0);
    assert_eq!(sequential["met"], true);
}

#[test]
fn fixed_is_the_default_method() {
    let explicit = run_count(&[
        "--debug-mode",
        "--target-precision=1%",
        "--target-precision-method=fixed",
        "sleep 0.1",
    ]);
    assert_eq!(
        explicit,
        run_count(&["--debug-mode", "--target-precision=1%", "sleep 0.1"])
    );
}

#[test]
fn the_sequential_method_has_requirements() {
    perfratio()
        .args([
            "--debug-mode",
            "--target-precision-method=sequential",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--target-precision"));
    for extra in [["--estimator", "median"], ["--subtract", "sleep 0.01"]] {
        perfratio()
            .args([
                "--debug-mode",
                "--target-precision=5%",
                "--target-precision-method=sequential",
            ])
            .args(extra)
            .arg("sleep 0.1")
            .assert()
            .failure()
            .stderr(predicate::str::contains("sequential"));
    }
}

#[test]
fn the_sequential_method_needs_more_runs_on_noisy_commands() {
    // Simulated noise: `--aggregate-parameter-runs` pools the 10, 20, 30 and
    // 40 ms runs into one result, so the spread is fixed by construction and
    // the run counts don't depend on the machine's load (real random sleeps
    // were flaky on macOS CI). The confidence sequence is wider than the
    // fixed interval, so it needs more runs to reach the same target.
    let noisy = [
        "--debug-mode",
        "--max-runs=400",
        "--min-runs=5",
        "--target-precision=15%",
        "--parameter-list",
        "d",
        "0.01,0.02,0.03,0.04",
        "--aggregate-parameter-runs",
        "sleep {d}",
    ];
    let fixed = result(&noisy);
    let sequential = result(&[&noisy[..], &["--target-precision-method=sequential"]].concat());
    assert_eq!(fixed["precision"]["met"], true);
    assert_eq!(sequential["precision"]["met"], true);
    let runs = |r: &Value| r["times"].as_array().unwrap().len();
    let (fixed, sequential) = (runs(&fixed), runs(&sequential));
    assert!(sequential > fixed, "{sequential} vs {fixed}");
}

#[test]
fn metrics_are_reported_and_a_metric_that_cannot_be_measured_is_not_waited_for() {
    // --debug-mode: constant wall time and memory, no energy sensor
    let r = result(&[
        "--debug-mode",
        "--target-precision=1%",
        "--target-precision-metrics=wall,memory,energy",
        "sleep 0.1",
    ]);
    let metrics = r["precision_metrics"].as_array().unwrap();
    assert_eq!(metrics.len(), 3);
    assert_eq!(metrics[0]["metric"], "wall");
    assert_eq!(metrics[0]["met"], true);
    assert_eq!(metrics[0]["reached"], 0.0);
    assert_eq!(metrics[1]["metric"], "memory");
    assert_eq!(metrics[1]["runs"], 10);
    assert_eq!(metrics[2]["metric"], "energy");
    assert_eq!(metrics[2]["runs"], 0);
    assert_eq!(metrics[2]["met"], false);
    // stops at --min-runs instead of waiting for energy until the time budget
    assert_eq!(r["times"].as_array().unwrap().len(), 10);
    // the single-metric field is unchanged
    assert_eq!(r["precision"]["met"], true);
}

#[test]
fn metrics_are_only_reported_when_requested() {
    assert!(
        result(&["--debug-mode", "--target-precision=1%", "sleep 0.1"])
            .get("precision_metrics")
            .is_none()
    );
}

#[test]
fn metrics_dedupe_and_reject_unknown_names() {
    let r = result(&[
        "--debug-mode",
        "--target-precision=1%",
        "--target-precision-metrics=memory,memory",
        "sleep 0.1",
    ]);
    assert_eq!(r["precision_metrics"].as_array().unwrap().len(), 1);

    perfratio()
        .args([
            "--target-precision=1%",
            "--target-precision-metrics=wall,cpu",
            "true",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--target-precision-metrics"));
    perfratio()
        .args(["--target-precision-metrics=wall", "true"])
        .assert()
        .failure();
}
