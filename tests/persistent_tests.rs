//! `--persistent` (#370, `docs/specs/persistent-mode.md`).
//!
//! The `--debug-mode` tests run everywhere. The others drive a shell script that plays a
//! persistent process (the wire format with fabricated, deterministic time stamps), so they
//! are Unix only. The real SDKs are tested in `sdk_tests.rs`.

mod common;
use common::perfratio;

use serde_json::Value;

fn json_of(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .args(["--export-json", path.to_str().unwrap()])
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn debug_mode_detects_the_simulated_warmup() {
    let json = json_of(&[
        "--debug-mode",
        "--persistent",
        "-w0",
        "--runs=20",
        "sleep 0.1",
    ]);
    let result = &json["results"][0];
    assert_eq!(result["mode"], "persistent");
    let it = &result["iterations"];
    assert_eq!(it["protocol"], 1);
    assert_eq!(it["count"], 20);
    assert_eq!(it["status"], "steady_state");
    assert_eq!(it["warmup_iterations"], 3);
    let steady = &it["steady_state"];
    assert_eq!(steady["first_iteration"], 3);
    assert_eq!(steady["count"], 17);
    assert!((steady["mean"].as_f64().unwrap() - 0.1).abs() < 1e-9);
    // the first iterations are the slow ones: 4x, 2x, 1.5x
    let times = result["times"].as_array().unwrap();
    assert!((times[0].as_f64().unwrap() - 0.4).abs() < 1e-9);
    assert!((times[3].as_f64().unwrap() - 0.1).abs() < 1e-9);
}

#[test]
fn a_run_without_persistent_has_no_mode() {
    let json = json_of(&["--debug-mode", "--runs=20", "sleep 0.1"]);
    assert!(json["results"][0].get("mode").is_none());
    assert!(json["results"][0].get("iterations").is_none());
}

#[test]
fn few_iterations_are_not_analyzed() {
    let json = json_of(&[
        "--debug-mode",
        "--persistent",
        "-w0",
        "--runs=5",
        "sleep 0.1",
    ]);
    let it = &json["results"][0]["iterations"];
    assert_eq!(it["status"], "too_few_iterations");
    assert!(it.get("steady_state").is_none());
}

#[test]
fn persistent_conflicts_with_modes_it_cannot_combine_with() {
    for flag in [
        "--region",
        "--concurrency=2",
        "--jobs=2",
        "--energy",
        "--until=x",
    ] {
        perfratio()
            .args(["--debug-mode", "--persistent", flag, "sleep 0.1"])
            .assert()
            .failure();
    }
}

#[cfg(unix)]
mod process {
    use super::*;
    use std::path::Path;

    /// A fake persistent program. `$PF_STARTS` receives one line per start. Iteration `n`
    /// lasts `slow` ms for the first `slow_count` iterations and `fast` ms afterwards.
    fn script(dir: &Path, hello: &str, body: &str) -> String {
        let path = dir.join("fake.sh");
        let text = format!(
            r#"#!/bin/sh
echo started >> "$PF_STARTS"
echo "noise on stdout that perfratio must ignore"
{hello}
while read cmd n; do
  case "$cmd" in
    quit) exit 0 ;;
    run)
{body}
      ;;
  esac
done
"#
        );
        std::fs::write(&path, text).unwrap();
        format!("sh {}", path.display())
    }

    const GOOD_HELLO: &str = r#"echo "@perfratio hello 1 shell/0.1.0""#;
    // iterations 0..4 last 40 ms, the rest 10 ms, at an arbitrary clock offset
    const GOOD_BODY: &str = r#"      base=$((5000000000 + n * 100000000))
      if [ "$n" -lt 4 ]; then dur=40000000; else dur=10000000; fi
      echo "@perfratio start iteration $base"
      echo "@perfratio stop iteration $((base + dur))"
      echo "@perfratio done $n""#;

    fn run(command: &str, extra: &[&str], starts: &Path) -> assert_cmd::assert::Assert {
        perfratio()
            .env("PF_STARTS", starts)
            .args(["--persistent", "-w", "0"])
            .args(extra)
            .arg(command)
            .assert()
    }

    #[test]
    fn one_process_serves_all_iterations() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let command = script(dir.path(), GOOD_HELLO, GOOD_BODY);
        let out = dir.path().join("out.json");
        run(
            &command,
            &["--runs=30", "--export-json", out.to_str().unwrap()],
            &starts,
        )
        .success();
        let started = std::fs::read_to_string(&starts).unwrap();
        assert_eq!(started.lines().count(), 1, "the command must start once");
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let result = &json["results"][0];
        assert_eq!(result["mode"], "persistent");
        let times: Vec<f64> = result["times"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_f64().unwrap())
            .collect();
        assert_eq!(times.len(), 30);
        assert!(
            times[..4].iter().all(|t| (t - 0.04).abs() < 1e-9),
            "{times:?}"
        );
        assert!(
            times[4..].iter().all(|t| (t - 0.01).abs() < 1e-9),
            "{times:?}"
        );
        let it = &result["iterations"];
        assert_eq!(it["sdk"], "shell/0.1.0");
        assert_eq!(it["status"], "steady_state");
        assert_eq!(it["warmup_iterations"], 4);
        assert_eq!(it["steady_state"]["count"], 26);
        assert!((it["steady_state"]["mean"].as_f64().unwrap() - 0.01).abs() < 1e-9);
        assert!(it["steady_state"]["stddev"].as_f64().unwrap() < 1e-12);
    }

    #[test]
    fn warmup_runs_are_extra_iterations_of_the_same_process() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let command = script(dir.path(), GOOD_HELLO, GOOD_BODY);
        perfratio()
            .env("PF_STARTS", &starts)
            .args(["--persistent", "-w", "3", "--runs=12", &command])
            .assert()
            .success();
        assert_eq!(std::fs::read_to_string(&starts).unwrap().lines().count(), 1);
    }

    #[test]
    fn a_program_that_speaks_another_protocol_version_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let command = script(dir.path(), r#"echo "@perfratio hello 2 old/9""#, GOOD_BODY);
        run(&command, &["--runs=3"], &starts)
            .failure()
            .stderr(predicates::str::contains("protocol mismatch"));
    }

    #[test]
    fn an_uninstrumented_command_fails_with_an_actionable_error() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        run("true", &["--runs=3"], &starts)
            .failure()
            .stderr(predicates::str::contains("did not say 'hello'"));
    }

    #[test]
    fn a_response_without_an_iteration_interval_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let command = script(dir.path(), GOOD_HELLO, r#"      echo "@perfratio done $n""#);
        run(&command, &["--runs=3"], &starts)
            .failure()
            .stderr(predicates::str::contains("no 'iteration' interval"));
    }

    #[test]
    fn a_failed_iteration_is_an_error_unless_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let body = r#"      echo "@perfratio fail $n boom""#;
        let command = script(dir.path(), GOOD_HELLO, body);
        run(&command, &["--runs=3"], &starts)
            .failure()
            .stderr(predicates::str::contains("failed iteration: boom"));
        run(&command, &["--runs=3", "-i"], &starts).success();
    }

    #[test]
    fn a_failed_iteration_is_measured_not_counted_as_zero() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let out = dir.path().join("out.json");
        // odd iterations fail: one reports its interval (5 ms), the others none
        let body = r#"      if [ $((n % 2)) -eq 1 ]; then
        echo "@perfratio fail $n boom"
      else
        echo "@perfratio start iteration 1000000000"
        echo "@perfratio stop iteration 1010000000"
        echo "@perfratio done $n"
      fi"#;
        let command = script(dir.path(), GOOD_HELLO, body);
        run(
            &command,
            &["--runs=6", "-i", "--export-json", out.to_str().unwrap()],
            &starts,
        )
        .success();
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let times = json["results"][0]["times"].as_array().unwrap();
        assert_eq!(times.len(), 6);
        assert!(
            times.iter().all(|t| t.as_f64().unwrap() > 0.0),
            "a failed iteration must not be recorded as 0.0: {times:?}"
        );
        assert!(json["results"][0]["min"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn grandchildren_are_killed_when_the_program_exits_by_itself() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let pidfile = dir.path().join("grandchild.pid");
        let hello = format!(
            "sleep 60 >/dev/null 2>&1 &\necho $! > {}\n{GOOD_HELLO}",
            pidfile.display()
        );
        let command = script(dir.path(), &hello, GOOD_BODY);
        run(&command, &["--runs=3"], &starts).success();
        let pid = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        let alive = || {
            let out = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", &pid])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
            !stat.is_empty() && !stat.starts_with('Z')
        };
        for _ in 0..50 {
            if !alive() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = std::process::Command::new("kill").arg(&pid).status();
        panic!("the grandchild {pid} survived the persistent process");
    }

    #[test]
    fn a_process_that_dies_mid_benchmark_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let body = r#"      if [ "$n" -ge 2 ]; then exit 7; fi
      echo "@perfratio start iteration 1"
      echo "@perfratio stop iteration 2"
      echo "@perfratio done $n""#;
        let command = script(dir.path(), GOOD_HELLO, body);
        run(&command, &["--runs=5"], &starts)
            .failure()
            .stderr(predicates::str::contains("exited"));
    }

    #[test]
    fn a_hung_iteration_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let starts = dir.path().join("starts");
        let command = script(dir.path(), GOOD_HELLO, "      sleep 5");
        run(&command, &["--runs=3", "--timeout=1"], &starts)
            .failure()
            .stderr(predicates::str::contains("did not answer in time"));
    }
}
