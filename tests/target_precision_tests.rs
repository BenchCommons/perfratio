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

#[cfg(unix)]
#[test]
fn the_sequential_method_needs_more_runs_on_noisy_commands() {
    // Random 10-50 ms sleeps: the confidence sequence is wider than the fixed interval
    let noisy = "sleep 0.0$((RANDOM % 4 + 1))";
    let shell = ["--shell=bash", "--max-runs=400", "--min-runs=5"];
    let fixed = run_count(&[&shell[..], &["--target-precision=15%", noisy]].concat());
    let sequential = run_count(
        &[
            &shell[..],
            &[
                "--target-precision=15%",
                "--target-precision-method=sequential",
                noisy,
            ],
        ]
        .concat(),
    );
    assert!(sequential > fixed, "{sequential} vs {fixed}");
}
