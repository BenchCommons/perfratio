//! Tests for `--expect-output-sha256`, `--validate`, `--validate-first-only`
//! and `--on-invalid` (#316, `docs/specs/workload-validation.md`).
//!
//! The portable tests use `--debug-mode`, whose simulated stdout is the words
//! after the fifth of `sleep <s> <user> <system> <memory> <words>`.

mod common;
use common::perfratio;

use predicates::prelude::*;

/// SHA-256 of "hello\n"
const HELLO: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

fn export<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["results"][0].clone()
}

/// Five simulated runs whose output is `good`, except `bad` at the given run
fn flipping(bad_run: usize) -> Vec<String> {
    let values: Vec<&str> = (0..5)
        .map(|i| if i == bad_run { "bad" } else { "good" })
        .collect();
    vec![
        "--debug-mode".into(),
        "--runs=1".into(),
        "--aggregate-parameter-runs".into(),
        "-L".into(),
        "w".into(),
        values.join(","),
        "sleep 0.01 0 0 0 {w}".into(),
    ]
}

fn good_hash() -> String {
    perfratio::validation::sha256_hex(b"good\n")
}

#[test]
fn matching_hash_is_recorded_in_the_export() {
    let result = export(&[
        "--debug-mode",
        "--runs=3",
        "--expect-output-sha256",
        HELLO,
        "sleep 0.01 0 0 0 hello",
    ]);
    let v = &result["validation"];
    assert_eq!(v["mode"], "output_sha256");
    assert_eq!(v["on_invalid"], "fail");
    assert_eq!(v["runs_checked"], 3);
    assert_eq!(v["runs_invalid"], 0);
    assert_eq!(v["runs_omitted"], 0);
    assert_eq!(v["output_sha256"], HELLO);
}

#[test]
fn no_validation_object_without_the_options() {
    let result = export(&["--debug-mode", "--runs=2", "sleep 0.01"]);
    assert!(result.get("validation").is_none());
}

#[test]
fn wrong_output_fails_naming_the_run_and_both_hashes() {
    let wrong = "0".repeat(64);
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--expect-output-sha256",
            &wrong,
            "sleep 0.01 0 0 0 hello",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Output validation failed"))
        .stderr(predicate::str::contains("iteration 0"))
        .stderr(predicate::str::contains(format!(
            "expected output sha256 {wrong}"
        )))
        .stderr(predicate::str::contains(format!("got {HELLO}")));
}

#[test]
fn a_malformed_hash_is_rejected() {
    perfratio()
        .args([
            "--debug-mode",
            "--expect-output-sha256",
            "abc",
            "sleep 0.01",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("64 hexadecimal digits"));
}

#[test]
fn on_invalid_and_first_only_need_a_check() {
    perfratio()
        .args(["--debug-mode", "--on-invalid=omit", "sleep 0.01"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--expect-output-sha256"));
}

#[test]
fn validation_conflicts_with_output_options() {
    perfratio()
        .args([
            "--debug-mode",
            "--show-output",
            "--expect-output-sha256",
            HELLO,
            "sleep 0.01",
        ])
        .assert()
        .failure();
}

#[test]
fn omit_drops_invalid_runs_loudly_and_exports_them() {
    let hash = good_hash();
    let mut args = flipping(2);
    args.extend([
        "--on-invalid=omit".into(),
        "--expect-output-sha256".into(),
        hash,
    ]);
    let result = export(&args);
    let v = &result["validation"];
    assert_eq!(v["runs_checked"], 5);
    assert_eq!(v["runs_invalid"], 1);
    assert_eq!(v["runs_omitted"], 1);
    assert_eq!(v["omitted_runs"], serde_json::json!([2]));
    // times stay aligned with the exit codes of the kept runs
    assert_eq!(result["times"].as_array().unwrap().len(), 4);
    assert_eq!(result["exit_codes"].as_array().unwrap().len(), 4);
}

#[test]
fn omit_prints_the_count() {
    let hash = good_hash();
    let mut args = flipping(1);
    args.extend([
        "--on-invalid=omit".into(),
        "--expect-output-sha256".into(),
        hash,
    ]);
    perfratio()
        .args(&args)
        .assert()
        .success()
        .stdout(predicate::str::contains("1 invalid runs omitted"))
        .stderr(predicate::str::contains(
            "1 of 5 benchmark runs that failed output validation",
        ));
}

#[test]
fn keep_flags_but_does_not_drop() {
    let hash = good_hash();
    let mut args = flipping(3);
    args.extend([
        "--on-invalid=keep".into(),
        "--expect-output-sha256".into(),
        hash,
    ]);
    let result = export(&args);
    let v = &result["validation"];
    assert_eq!(v["runs_invalid"], 1);
    assert_eq!(v["runs_omitted"], 0);
    assert!(v.get("omitted_runs").is_none());
    assert_eq!(result["times"].as_array().unwrap().len(), 5);
}

#[test]
fn fail_aborts_at_the_flipped_run() {
    let hash = good_hash();
    let mut args = flipping(2);
    args.extend(["--expect-output-sha256".into(), hash]);
    perfratio()
        .args(&args)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Output validation failed"));
}

#[test]
fn all_invalid_with_omit_is_an_error() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--on-invalid=omit",
            "--expect-output-sha256",
            &"0".repeat(64),
            "sleep 0.01 0 0 0 hello",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed output validation"));
}

#[test]
fn first_only_checks_one_run() {
    let result = export(&[
        "--debug-mode",
        "--runs=4",
        "--validate-first-only",
        "--expect-output-sha256",
        HELLO,
        "sleep 0.01 0 0 0 hello",
    ]);
    let v = &result["validation"];
    assert_eq!(v["first_only"], true);
    assert_eq!(v["runs_checked"], 1);
}

#[cfg(unix)]
mod unix {
    use super::*;

    #[test]
    fn real_command_output_is_hashed() {
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let result = export(&["--runs=3", "--expect-output-sha256", sha, "printf abc"]);
        assert_eq!(result["validation"]["runs_checked"], 3);
        assert_eq!(result["validation"]["output_sha256"], sha);
        perfratio()
            .args(["--runs=3", "--expect-output-sha256", sha, "printf abd"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("Output validation failed"));
    }

    #[test]
    fn validator_failure_is_an_invalid_run_not_an_exit_code_failure() {
        // the run number is the command's output; runs 2 and later are invalid
        let args = [
            "--runs=4",
            "--on-invalid=omit",
            "--validate",
            "test \"$(cat)\" -lt 2",
            "printf %s \"$PERFRATIO_ITERATION\"",
        ];
        let result = export(&args);
        let v = &result["validation"];
        assert_eq!(v["mode"], "command");
        assert_eq!(v["runs_invalid"], 2);
        assert_eq!(v["runs_omitted"], 2);
        assert_eq!(v["omitted_runs"], serde_json::json!([2, 3]));
        assert!(result["exit_codes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c == 0));
    }

    #[test]
    fn validator_receives_the_run_id() {
        let result = export(&[
            "--runs=3",
            "--validate",
            "test \"$PERFRATIO_RUN_ID\" = \"$(cat)\"",
            "printf %s \"$PERFRATIO_ITERATION\"",
        ]);
        assert_eq!(result["validation"]["runs_invalid"], 0);
    }

    #[test]
    fn validation_time_is_excluded_from_the_measurement() {
        let result = export(&["--runs=3", "--validate", "sleep 0.4", "true"]);
        let mean = result["mean"].as_f64().unwrap();
        assert!(mean < 0.2, "validator time leaked into the mean: {mean}");
        assert_eq!(result["validation"]["runs_checked"], 3);
    }

    #[test]
    fn failed_commands_are_not_validated() {
        let result = export(&[
            "--runs=3",
            "--ignore-failure",
            "--expect-output-sha256",
            &"0".repeat(64),
            "false",
        ]);
        assert_eq!(result["validation"]["runs_checked"], 0);
    }
}
