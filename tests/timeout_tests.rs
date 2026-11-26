//! Tests for --timeout (#51).

mod common;
use common::perfratio;

use predicates::prelude::*;
use serde_json::Value;

#[test]
fn invalid_timeout_durations_are_rejected() {
    for invalid in ["0", "0s", "0ms", "-1s", "abc", "inf", "NaN", "-500ms"] {
        perfratio()
            .args(["--timeout", invalid, "sleep 0.01"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Invalid duration"));
    }
}

#[test]
fn timed_out_benchmark_skips_remaining_runs_and_reports_timeout() {
    let start = std::time::Instant::now();
    perfratio()
        .args([
            "--timeout",
            "150ms",
            "-N",
            "--runs=3",
            "--style=basic",
            "sleep 5",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Timed out"))
        .stdout(predicate::str::contains("skipped remaining runs"));

    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "Timed out benchmark took too long: {:?}",
        start.elapsed()
    );
}

#[test]
fn timeout_with_multiple_commands_and_relative_speed() {
    let start = std::time::Instant::now();
    perfratio()
        .args([
            "--timeout",
            "150ms",
            "-N",
            "--runs=2",
            "--style=basic",
            "sleep 5",
            "sleep 0.01",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Timed out"))
        .stdout(predicate::str::contains("(timeout)"))
        .stdout(predicate::str::contains("sleep 0.01"));

    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "Multiple command benchmark took too long: {:?}",
        start.elapsed()
    );
}

#[test]
fn timeout_json_export() {
    let dir = tempfile::tempdir().unwrap();
    let json_file = dir.path().join("out.json");

    perfratio()
        .args(["--timeout", "150ms", "-N", "--runs=2", "--export-json"])
        .arg(&json_file)
        .args(["sleep 5", "sleep 0.01"])
        .assert()
        .success();

    let content = std::fs::read_to_string(&json_file).unwrap();
    let json: Value = serde_json::from_str(&content).unwrap();
    let results = json["results"].as_array().unwrap();

    assert_eq!(results.len(), 2);

    // Command 1: timed out on first run
    let cmd1 = &results[0];
    assert_eq!(cmd1["command"], "sleep 5");
    assert_eq!(cmd1["timed_out"], true);
    assert!((cmd1["timeout"].as_f64().unwrap() - 0.15).abs() < 1e-6);
    assert_eq!(cmd1["times"].as_array().unwrap().len(), 0);

    // Command 2: completed normally
    let cmd2 = &results[1];
    assert_eq!(cmd2["command"], "sleep 0.01");
    assert!(cmd2.get("timed_out").is_none() || cmd2["timed_out"] == false);
    assert_eq!(cmd2["times"].as_array().unwrap().len(), 2);
}

#[test]
fn timeout_csv_and_markdown_exports() {
    let dir = tempfile::tempdir().unwrap();
    let csv_file = dir.path().join("out.csv");
    let md_file = dir.path().join("out.md");

    perfratio()
        .args(["--timeout", "200ms", "-N", "--runs=2", "--export-csv"])
        .arg(&csv_file)
        .arg("--export-markdown")
        .arg(&md_file)
        .args(["sleep 5", "sleep 0.01"])
        .assert()
        .success();

    // Verify CSV output
    let csv_content = std::fs::read_to_string(&csv_file).unwrap();
    assert!(
        csv_content.contains(">0.200 (timeout)"),
        "CSV should contain '>0.200 (timeout)':\n{csv_content}"
    );

    // Verify Markdown output
    let md_content = std::fs::read_to_string(&md_file).unwrap();
    assert!(
        md_content.contains("(timeout)"),
        "Markdown should contain '(timeout)':\n{md_content}"
    );
    assert!(
        md_content.contains("n/a"),
        "Markdown relative column should be 'n/a' for timed out command:\n{md_content}"
    );
}

#[test]
fn intermediate_preparation_timeout_aborts_with_error() {
    perfratio()
        .args(["--timeout", "150ms", "--prepare", "sleep 5", "sleep 0.01"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "The preparation command timed out",
        ));
}

#[test]
fn intermediate_setup_timeout_aborts_with_error() {
    perfratio()
        .args(["--timeout", "150ms", "--setup", "sleep 5", "sleep 0.01"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("The setup command timed out"));
}

#[cfg(unix)]
#[test]
fn process_group_kills_descendants_on_unix() {
    // Spawn a subshell that launches a background sleep and waits
    // Watchdog should kill the entire process group including the background sleep
    let start = std::time::Instant::now();
    perfratio()
        .args(["--timeout", "200ms", "--runs=1", "sh -c 'sleep 30 & wait'"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Timed out"));

    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "Process tree kill took too long: {:?}",
        start.elapsed()
    );
}

// TEMPORARY CI diagnostic (draft PR, not for merge)
#[cfg(target_os = "linux")]
#[test]
fn zz_diag_timeout_timing() {
    let mut report = String::new();
    for (label, fake) in [("real", None), ("emptyroot", Some("/nonexistent-root"))] {
        for args in [
            vec!["--timeout", "150ms", "-N", "--runs=2", "--style=basic", "sleep 5", "sleep 0.01"],
            vec!["-N", "--runs=2", "--style=basic", "sleep 0.01"],
            vec!["-N", "--runs=2", "--style=basic", "--calibrate=off", "sleep 0.01"],
        ] {
            let mut cmd = std::process::Command::new(assert_cmd::cargo_bin!("perfratio"));
            cmd.args(&args);
            if let Some(f) = fake { cmd.env("PERFRATIO_SYSTEM_CHECK_ROOT", f); }
            let t = std::time::Instant::now();
            let out = cmd.output().unwrap();
            report.push_str(&format!("{label} {:?} -> {:?} status={:?}\n", args, t.elapsed(), out.status.code()));
        }
    }
    let t = std::time::Instant::now();
    let w = perfratio::environment_watch::Watch::new(std::path::Path::new("/"));
    let new_t = t.elapsed();
    let t = std::time::Instant::now();
    let r = w.finish();
    report.push_str(&format!("Watch::new {new_t:?} finish {:?} -> {r:?}\n", t.elapsed()));
    let t = std::time::Instant::now();
    let c = perfratio::cgroup::locate(std::path::Path::new("/"));
    report.push_str(&format!("cgroup::locate {:?} -> {c:?}\n", t.elapsed()));
    panic!("DIAG\n{report}");
}
