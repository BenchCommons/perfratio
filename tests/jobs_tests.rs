//! Tests for --jobs (#306). `--debug-mode` makes the runs deterministic; the
//! Unix tests use real `sleep` and shell commands with wide timing margins.

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
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn run_counts(json: &Value) -> Vec<usize> {
    json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["times"].as_array().unwrap().len())
        .collect()
}

#[test]
fn invalid_values_are_rejected() {
    for invalid in ["0", "abc", "-1", "1.5", ""] {
        perfratio()
            .args(["--debug-mode", &format!("--jobs={invalid}"), "sleep 0.1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("--jobs"));
    }
}

#[test]
fn incompatible_options_are_rejected() {
    for other in [
        &["--concurrency", "2"][..],
        &["--energy"],
        &["--subtract", "sleep 0.01"],
        &["--optimize", "n", "1", "10"],
    ] {
        perfratio()
            .args(["--debug-mode", "--jobs", "2"])
            .args(other)
            .arg("sleep {n}")
            .assert()
            .failure();
    }
}

#[test]
fn schedule_must_match_jobs() {
    for schedule in ["saturate", "exclusive", "fair"] {
        perfratio()
            .args(["--debug-mode", "--schedule", schedule, "sleep 0.1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("needs '--jobs' > 1"));
    }
    for schedule in ["grouped", "round-robin"] {
        perfratio()
            .args(["--debug-mode", "-j2", "--schedule", schedule, "sleep 0.1"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("saturate"));
    }
    perfratio()
        .args(["--debug-mode", "-j2", "--round-robin", "sleep 0.1"])
        .assert()
        .failure();
}

#[test]
fn jobs_one_is_identical_to_the_default() {
    let plain = export(&["--debug-mode", "--runs=3", "sleep 0.2"]);
    let one = export(&["--debug-mode", "--runs=3", "--jobs", "1", "sleep 0.2"]);
    assert_eq!(plain["results"][0]["times"], one["results"][0]["times"]);
    assert!(one["perfratio"].get("execution").is_none());
    for key in ["workers_used", "contended"] {
        assert!(one["results"][0].get(key).is_none(), "{key}");
    }
}

#[test]
fn json_records_the_execution_and_marks_results_contended() {
    for (args, policy) in [
        (&["-j", "4"][..], "saturate"),
        (&["-j", "4", "--schedule", "exclusive"], "exclusive"),
        (&["--jobs=3", "--schedule=fair"], "fair"),
    ] {
        let json = export(
            &[
                &["--debug-mode", "--runs=10"],
                args,
                &["sleep 0.1", "sleep 0.2"],
            ]
            .concat(),
        );
        assert_eq!(json["perfratio"]["execution"]["schedule_policy"], policy);
        assert_eq!(json["perfratio"]["execution"]["contended"], true);
        assert!(json["perfratio"]["execution"]["jobs"].as_u64().unwrap() >= 3);
        for result in json["results"].as_array().unwrap() {
            assert_eq!(result["contended"], true);
            let workers = result["workers_used"].as_u64().unwrap();
            assert!((1..=4).contains(&workers), "{workers}");
        }
    }
}

#[test]
fn the_number_of_recorded_runs_equals_runs_for_every_policy() {
    for schedule in ["saturate", "exclusive", "fair"] {
        for jobs in ["2", "5", "auto"] {
            let json = export(&[
                "--debug-mode",
                "--runs=11",
                "--jobs",
                jobs,
                "--schedule",
                schedule,
                "sleep 0.1",
                "sleep 0.2",
                "sleep 0.3",
            ]);
            assert_eq!(run_counts(&json), [11, 11, 11], "{schedule} {jobs}");
        }
    }
}

#[test]
fn a_single_command_works_and_statistics_are_per_run() {
    let json = export(&["--debug-mode", "--runs=20", "-j4", "sleep 0.1"]);
    let r = &json["results"][0];
    assert_eq!(r["times"].as_array().unwrap().len(), 20);
    assert_eq!(r["exit_codes"].as_array().unwrap().len(), 20);
    assert!(r["stddev"].is_number());
    assert!(r.get("concurrency").is_none());
}

#[test]
fn target_precision_and_duration_work_with_jobs() {
    let json = export(&[
        "--debug-mode",
        "--max-runs=30",
        "--target-precision=1%",
        "-j3",
        "sleep 0.1",
    ]);
    let n = run_counts(&json)[0];
    assert!((2..=30).contains(&n), "{n}");
    let json = export(&["--debug-mode", "--duration=2s", "-j3", "sleep 0.1"]);
    assert!(run_counts(&json)[0] >= 10);
}

#[test]
fn exported_json_can_be_imported_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jobs.json");
    perfratio()
        .args(["--debug-mode", "--runs=4", "-j2", "sleep 0.1"])
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    perfratio()
        .arg("--import-json")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn the_terminal_warns_that_parallel_sampling_perturbs_timing() {
    perfratio()
        .args(["--debug-mode", "--runs=3", "-j2", "sleep 0.1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("contended"));
}

#[cfg(unix)]
#[test]
fn real_runs_overlap() {
    let start = std::time::Instant::now();
    let json = export(&[
        "-N",
        "--runs=8",
        "-j4",
        "--pin-workers=none",
        "--style=basic",
        "sleep 0.2",
    ]);
    let elapsed = start.elapsed().as_secs_f64();
    // 8 sequential runs would take 1.6 s
    assert!(elapsed < 1.4, "took {elapsed} s");
    assert_eq!(run_counts(&json), [8]);
    // every run is timed on its own, not as a batch
    let mean = json["results"][0]["mean"].as_f64().unwrap();
    assert!((0.19..0.4).contains(&mean), "mean {mean}");
}

#[cfg(unix)]
#[test]
fn exclusive_never_overlaps_a_command_with_itself() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().display();
    // `mkdir` fails when the lock directory exists, i.e. when two runs of the
    // same command overlap
    let a = format!("mkdir {d}/a && sleep 0.1 && rmdir {d}/a");
    let b = format!("mkdir {d}/b && sleep 0.1 && rmdir {d}/b");
    let json = export(&[
        "--runs=6",
        "-j4",
        "--pin-workers=none",
        "--schedule=exclusive",
        "--style=basic",
        &a,
        &b,
    ]);
    assert_eq!(run_counts(&json), [6, 6]);
    // the negative control: saturate overlaps the runs of one command
    perfratio()
        .args([
            "--runs=6",
            "-j4",
            "--schedule=saturate",
            "--style=basic",
            &a,
        ])
        .assert()
        .failure();
}

#[cfg(unix)]
#[test]
fn commands_see_their_worker_slot() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("slots");
    let command = format!(
        "echo $PERFRATIO_WORKER_ID:$PERFRATIO_RUN_ID >> {}; sleep 0.05",
        log.display()
    );
    perfratio()
        .args([
            "--runs=9",
            "-j3",
            "--pin-workers=none",
            "--style=basic",
            &command,
        ])
        .assert()
        .success();
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 9, "{text}");
    let (slots, ids): (Vec<&str>, Vec<&str>) =
        lines.iter().map(|l| l.split_once(':').unwrap()).unzip();
    // run 0 is the sequential initial measurement, the other 8 ran on workers
    assert_eq!(slots.iter().filter(|s| s.is_empty()).count(), 1, "{text}");
    for slot in slots.iter().filter(|s| !s.is_empty()) {
        assert!(["0", "1", "2"].contains(slot), "{text}");
    }
    // every run has its own id
    let unique: std::collections::BTreeSet<&&str> = ids.iter().collect();
    assert_eq!(unique.len(), 9, "{text}");
}

#[cfg(unix)]
#[test]
fn ctrl_c_keeps_equal_run_counts_with_fair() {
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.json");
    let child = Command::new(assert_cmd::cargo_bin!("perfratio"))
        .args([
            "--runs=400",
            "-N",
            "-j3",
            "--schedule=fair",
            "--export-json",
        ])
        .arg(&path)
        .args(["sleep 0.1", "sleep 0.1", "sleep 0.1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perfratio");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    // SAFETY: plain kill(2) on the child we spawned
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(json["interrupted"], true);
    let counts = run_counts(&json);
    assert_eq!(counts.len(), 3, "{counts:?}");
    assert!(
        counts.iter().all(|&n| n == counts[0] && n > 1),
        "{counts:?}"
    );
}

#[test]
fn a_huge_job_count_does_not_start_that_many_workers() {
    let json = export(&["--debug-mode", "--runs=3", "--jobs=100000", "sleep 0.1"]);
    assert_eq!(run_counts(&json), [3]);
    let workers = json["results"][0]["workers_used"].as_u64().unwrap();
    assert!(workers <= 2, "{workers}");
}
