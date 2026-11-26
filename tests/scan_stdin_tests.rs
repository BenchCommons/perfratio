//! Logarithmic scans, commands from stdin, `--raw-ratios`, automatic names (#327)

mod common;
use common::perfratio;

use predicates::prelude::*;

fn debug() -> assert_cmd::Command {
    let mut cmd = perfratio();
    cmd.arg("--debug-mode");
    cmd
}

fn json_of(cmd: &mut assert_cmd::Command) -> serde_json::Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    cmd.arg(format!("--export-json={}", path.display()))
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn logarithmic_scan_values() {
    let mut cmd = debug();
    cmd.args([
        "--runs=1",
        "-P",
        "n",
        "10",
        "1000000",
        "--parameter-step-n",
        "5",
        "--parameter-step-log",
        "sleep {n}",
    ]);
    let json = json_of(&mut cmd);
    let values: Vec<String> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["parameters"]["n"].to_string())
        .collect();
    assert_eq!(
        values,
        [
            "\"10\"",
            "\"100\"",
            "\"1000\"",
            "\"10000\"",
            "\"100000\"",
            "\"1000000\""
        ]
        .map(String::from)
    );
}

#[test]
fn linear_step_count_and_rounding_note() {
    debug()
        .args([
            "--runs=1",
            "-P",
            "n",
            "0",
            "10",
            "--parameter-step-n",
            "3",
            "sleep {n}",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Note: '-P n': the scan values were rounded to integers",
        ));
}

#[test]
fn scan_spacing_errors() {
    for (args, message) in [
        (
            vec![
                "-P",
                "n",
                "0",
                "10",
                "--parameter-step-n",
                "3",
                "--parameter-step-log",
            ],
            "MIN > 0",
        ),
        (
            vec!["-P", "n", "1", "10", "--parameter-step-n", "0"],
            "between 1 and",
        ),
        (
            vec!["-P", "n", "1", "10", "--parameter-step-n", "x"],
            "positive integer",
        ),
        (
            vec!["-P", "n", "1", "10", "--parameter-step-n", "2", "-D", "1"],
            "cannot be used with",
        ),
        (vec!["--parameter-step-log", "sleep 1"], "required"),
    ] {
        debug()
            .args(args)
            .arg("sleep {n}")
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));
    }
}

#[test]
fn commands_from_stdin() {
    debug()
        .args(["--runs=1", "--style=basic", "-"])
        .write_stdin("sleep 0.1\n\n# a comment\n  sleep 0.2  \r\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Benchmark 1: sleep 0.1"))
        .stdout(predicate::str::contains("Benchmark 2: sleep 0.2"))
        .stdout(predicate::str::contains("Benchmark 3").not());
}

#[test]
fn empty_stdin_fails_clearly() {
    for input in ["", "\n  \n# only a comment\n"] {
        debug()
            .args(["--runs=1", "-"])
            .write_stdin(input)
            .assert()
            .failure()
            .stderr(predicate::str::contains("No commands on stdin"));
    }
}

#[test]
fn stdin_cannot_be_mixed_with_commands() {
    debug()
        .args(["--runs=1", "-", "sleep 1"])
        .write_stdin("sleep 1\n")
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be combined"));
}

#[test]
fn stdin_commands_take_parameters_and_names() {
    let mut cmd = debug();
    cmd.args(["--runs=1", "-L", "x", "1,2", "--command-name=run {x}", "-"])
        .write_stdin("sleep 0.{x}\n");
    let json = json_of(&mut cmd);
    assert_eq!(json["results"].as_array().unwrap().len(), 2);
    assert_eq!(json["results"][0]["command"], "run 1");
}

#[test]
fn raw_ratios_prints_only_the_ratios() {
    let out = debug()
        .args([
            "--runs=3",
            "--raw-ratios",
            "sleep 0.1",
            "sleep 0.2",
            "sleep 0.4",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout, "1.000000\n2.000000\n4.000000\n");
}

#[test]
fn raw_ratios_follow_the_reference() {
    debug()
        .args([
            "--runs=3",
            "--raw-ratios",
            "--reference=sleep 0.2",
            "sleep 0.1",
            "sleep 0.4",
        ])
        .assert()
        .success()
        .stdout("1.000000\n0.500000\n2.000000\n");
}

#[test]
fn raw_ratios_reject_stdout_exports_and_shown_output() {
    debug()
        .args(["--runs=1", "--raw-ratios", "--export-json=-", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("export to '-'"));
    debug()
        .args(["--runs=1", "--raw-ratios", "--show-output", "sleep 0.1"])
        .assert()
        .failure();
}

#[test]
fn automatic_names_strip_common_words() {
    let mut cmd = debug();
    cmd.args(["--runs=1", "sleep 0.1", "sleep 0.2", "--command-name"]);
    let json = json_of(&mut cmd);
    let names: Vec<&str> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["command"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["0.1", "0.2"]);
}

#[test]
fn automatic_names_with_equals_form_and_scan() {
    let mut cmd = debug();
    cmd.args([
        "--runs=1",
        "--command-name=",
        "-P",
        "n",
        "1",
        "3",
        "sleep {n}",
    ]);
    let json = json_of(&mut cmd);
    let names: Vec<&str> = json["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["command"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["1", "2", "3"]);
}

#[test]
fn automatic_names_fall_back_to_indices() {
    let mut cmd = debug();
    // nothing in common: two identical commands
    cmd.args(["--runs=1", "sleep 0.1", "sleep 0.1", "--command-name"]);
    let json = json_of(&mut cmd);
    assert_eq!(json["results"][0]["command"], "#1");
    assert_eq!(json["results"][1]["command"], "#2");
}

#[test]
fn explicit_and_automatic_names_do_not_mix() {
    debug()
        .args([
            "--runs=1",
            "-n",
            "a",
            "--command-name=",
            "sleep 1",
            "sleep 2",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot be combined with explicit names",
        ));
}
