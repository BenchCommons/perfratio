//! `--counters` (#49): CLI parsing, JSON and the unavailable path. `--debug-mode`
//! simulates deterministic counter values, so nothing here needs a PMU.

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
fn default_list_is_exported_per_run_with_summary_and_rates() {
    let json = export(&["--runs=3", "--counters", "sleep 0.5"]);
    assert_eq!(json["perfratio"]["measurements"]["counters"], "measured");
    let r = &json["results"][0];
    assert_eq!(r["counters_scope"], "all");
    let instr = r["counters"]["instructions"].as_array().unwrap();
    assert_eq!(instr.len(), r["times"].as_array().unwrap().len());
    assert_eq!(instr[0], 1_000_000_000u64);
    assert_eq!(r["counters"].as_object().unwrap().len(), 6);
    assert_eq!(r["counters_summary"]["cycles"]["mean"], 500_000_000.0);
    assert_eq!(r["efficiency"]["ipc"], 2.0);
    assert!(r.get("counters_multiplexed_runs").is_none());
}

#[test]
fn explicit_list_and_terminal_line() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--counters=cycles,instructions,cycles",
        ])
        .arg("sleep 0.1")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Counters (mean):    cycles 100.0M (IPC 2.00) @1.00 GHz · instr 200.0M",
        ));
    let json = export(&["--runs=2", "--counters=cycles", "sleep 0.1"]);
    let counters = json["results"][0]["counters"].as_object().unwrap();
    assert_eq!(counters.keys().collect::<Vec<_>>(), ["cycles"]);
}

#[test]
fn modifiers_suffix_the_keys_and_split_the_values() {
    let json = export(&[
        "--runs=2",
        "--counters=instructions:u,instructions:k,cycles",
        "sleep 0.5",
    ]);
    let r = &json["results"][0];
    assert_eq!(r["counters_scope"], "all");
    let c = r["counters"].as_object().unwrap();
    assert_eq!(
        c.keys().collect::<Vec<_>>(),
        ["cycles", "instructions:k", "instructions:u"]
    );
    let user = c["instructions:u"][0].as_u64().unwrap();
    let kernel = c["instructions:k"][0].as_u64().unwrap();
    assert_eq!(user + kernel, 1_000_000_000);
    assert_eq!(c["instructions:u"].as_array().unwrap().len(), 2);
    assert!(r["counters_summary"]["instructions:k"]["mean"].is_f64());
    let m = &json["perfratio"]["measurements"];
    assert_eq!(m["counters"], "measured");
    assert!(m.get("counters_requested_scope").is_none());
}

#[test]
fn scope_split_and_user_and_kernel() {
    let json = export(&[
        "--runs=1",
        "--counters=cycles",
        "--scope=split",
        "sleep 0.5",
    ]);
    let keys: Vec<_> = json["results"][0]["counters"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["cycles:k", "cycles:u"]);
    assert_eq!(json["results"][0]["counters_scope"], "all");

    let json = export(&["--runs=1", "--counters=cycles", "--scope=user", "sleep 0.5"]);
    let r = &json["results"][0];
    assert_eq!(r["counters_scope"], "user");
    assert_eq!(r["counters"]["cycles"][0], 125_000_000u64);

    let json = export(&[
        "--runs=1",
        "--counters=cycles",
        "--scope=kernel",
        "sleep 0.5",
    ]);
    let r = &json["results"][0];
    assert_eq!(r["counters_scope"], "kernel");
    assert_eq!(r["counters"]["cycles"][0], 375_000_000u64);
}

#[test]
fn split_terminal_line_shows_the_breakdown() {
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--counters=instructions,cycles",
            "--scope=split",
        ])
        .arg("sleep 0.1")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "instr 200.0M [user 50.0M, kernel 150.0M] · cycles 100.0M [user 25.0M, kernel 75.0M] (IPC 2.00) @1.00 GHz",
        ));
}

#[test]
fn bad_modifiers_and_scope_are_rejected() {
    perfratio()
        .args(["--debug-mode", "--counters=cycles:h", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("modifier ':h' is not supported"));
    perfratio()
        .args(["--debug-mode", "--counters=cycles:x", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown modifier ':x'"));
    perfratio()
        .args(["--debug-mode", "--counters", "--scope=both", "sleep 0.1"])
        .assert()
        .failure();
    perfratio()
        .args(["--debug-mode", "--scope=user", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--counters"));
}

#[test]
fn without_the_flag_nothing_is_added() {
    let json = export(&["--runs=2", "sleep 0.1"]);
    assert_eq!(
        json["perfratio"]["measurements"]["counters"],
        "not_requested"
    );
    assert!(json["results"][0].get("counters").is_none());
    assert!(json["results"][0].get("counters_scope").is_none());
}

#[test]
fn unknown_counter_lists_the_valid_names() {
    perfratio()
        .args(["--debug-mode", "--counters=instructions,bogus", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown counter 'bogus'"))
        .stderr(predicate::str::contains("branch-misses"));
}

#[test]
fn bare_flag_does_not_swallow_the_command() {
    perfratio()
        .args(["--debug-mode", "--runs=1", "--counters", "sleep 0.1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Benchmark 1: sleep 0.1"));
}

#[test]
fn imported_results_keep_counters() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("a.json");
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--counters=cycles",
            "--export-json",
        ])
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
    let b: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        b["results"][0]["counters"]["cycles"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
#[test]
fn unsupported_platform_warns_once_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--counters", "--export-json"])
        .arg(&json)
        .args(["--", env!("CARGO"), "--version"])
        .assert()
        .success()
        .stderr(predicate::str::contains("only supported on Linux").count(1));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let m = &v["perfratio"]["measurements"];
    assert_eq!(m["counters"], "unavailable");
    assert_eq!(m["counters_unavailable_reason"], "unsupported_platform");
    assert!(v["results"][0].get("counters").is_none());
}

/// macOS (#411): task-level counters, no root, only with `-N`. Apple Silicon only.
#[cfg(target_os = "macos")]
#[test]
fn macos_counts_instructions_and_cycles_with_no_shell() {
    if cfg!(not(target_arch = "aarch64")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args([
            "--runs=2",
            "--counters=instructions,cycles",
            "-N",
            "--export-json",
        ])
        .arg(&json)
        .args(["--", "/usr/bin/awk", "BEGIN{for(i=0;i<1000000;i++)s+=i}"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["perfratio"]["measurements"]["counters"], "measured");
    for name in ["instructions", "cycles"] {
        let runs = v["results"][0]["counters"][name].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().all(|x| x.as_u64().unwrap() > 0), "{name}");
    }
}

/// macOS (#411): with a shell the counters would measure the shell, so they
/// are dropped with a warning that points to `-N`.
#[cfg(target_os = "macos")]
#[test]
fn macos_with_a_shell_is_unavailable_and_suggests_no_shell() {
    if cfg!(not(target_arch = "aarch64")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--counters=instructions", "--export-json"])
        .arg(&json)
        .arg("true")
        .assert()
        .success()
        .stderr(predicate::str::contains("Use '-N'"));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let m = &v["perfratio"]["measurements"];
    assert_eq!(m["counters"], "unavailable");
    assert_eq!(m["counters_unavailable_reason"], "other");
    assert!(v["results"][0].get("counters").is_none());
}

/// Windows (#411): `cycles` from `QueryProcessCycleTime`, no admin, only with `-N`.
/// `instructions` has no source, so the result is partial.
#[cfg(windows)]
#[test]
fn windows_counts_cycles_with_no_shell() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args([
            "--runs=2",
            "--counters=cycles,instructions",
            "-N",
            "--export-json",
        ])
        .arg(&json)
        .args(["--", "cmd", "/C", "exit", "0"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["perfratio"]["measurements"]["counters"], "partial");
    let runs = v["results"][0]["counters"]["cycles"].as_array().unwrap();
    assert_eq!(runs.len(), 2);
    assert!(
        runs.iter().all(|x| x.as_u64().unwrap() > 0),
        "cycles per run: {runs:?}"
    );
    assert!(v["results"][0]["counters"].get("instructions").is_none());
}

/// Windows (#411): with a shell the counters would measure the shell, so they
/// are dropped with a warning that points to `-N`.
#[cfg(windows)]
#[test]
fn windows_with_a_shell_is_unavailable_and_suggests_no_shell() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--counters=cycles", "--export-json"])
        .arg(&json)
        .arg("echo ok")
        .assert()
        .success()
        .stderr(predicate::str::contains("Use '-N'"));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    let m = &v["perfratio"]["measurements"];
    assert_eq!(m["counters"], "unavailable");
    assert_eq!(m["counters_unavailable_reason"], "other");
    assert!(v["results"][0].get("counters").is_none());
}

/// Windows (#411): only `instructions` requested, which has no source.
#[cfg(windows)]
#[test]
fn windows_instructions_alone_are_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let json = dir.path().join("out.json");
    perfratio()
        .args(["--runs=2", "--counters=instructions", "-N", "--export-json"])
        .arg(&json)
        .args(["--", "cmd", "/C", "exit", "0"])
        .assert()
        .success()
        .stderr(predicate::str::contains("can only read cycles"));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
    assert_eq!(v["perfratio"]["measurements"]["counters"], "unavailable");
    assert!(v["results"][0].get("counters").is_none());
}

/// Real counters: needs a PMU, so only with `PERFRATIO_TEST_PERF=1`.
#[cfg(target_os = "linux")]
#[test]
fn real_instructions_are_counted() {
    if std::env::var_os("PERFRATIO_TEST_PERF").is_none() {
        return;
    }
    let json = {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("o.json");
        perfratio()
            .args(["--runs=5", "--counters=instructions", "-N", "--export-json"])
            .arg(&out)
            .args(["--", "seq", "100000"])
            .assert()
            .success();
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&out).unwrap()).unwrap()
    };
    assert_eq!(json["perfratio"]["measurements"]["counters"], "measured");
    assert!(
        json["results"][0]["counters_summary"]["instructions"]["mean"]
            .as_f64()
            .unwrap()
            > 1e6
    );
}

#[test]
fn software_events_are_exported_and_printed() {
    let json = export(&[
        "--runs=3",
        "--counters=cpu-migrations,context-switches,page-faults",
        "sleep 0.5",
    ]);
    let r = &json["results"][0];
    assert_eq!(
        r["counters"]["cpu-migrations"],
        serde_json::json!([0, 0, 0])
    );
    assert_eq!(r["counters"]["context-switches"][0], 50);
    assert_eq!(r["counters"]["page-faults"][0], 500);
    assert_eq!(r["counters_summary"]["context-switches"]["mean"], 50.0);
    // no cycles: no frequency
    assert!(r.get("effective_frequency_hz").is_none());
    perfratio()
        .args(["--debug-mode", "--runs=2", "--counters=sw"])
        .arg("sleep 0.5")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Counters (mean):    cpu-migr 0.0 · ctx-sw 50",
        ));
}

#[test]
fn frequency_is_exported_next_to_cycles() {
    let json = export(&["--runs=3", "--counters=frequency", "sleep 0.5"]);
    let r = &json["results"][0];
    assert_eq!(r["effective_frequency_basis"], "task_clock");
    assert_eq!(r["mean_effective_frequency_hz"], 1.0e9);
    let series = r["effective_frequency_hz"].as_array().unwrap();
    assert_eq!(series.len(), r["times"].as_array().unwrap().len());
    assert_eq!(series[0], 1.0e9);
    // the divisor is an implementation detail, not a counter
    let counters = r["counters"].as_object().unwrap();
    assert_eq!(counters.keys().collect::<Vec<_>>(), ["cycles"]);
    // the default list collects cycles too
    let json = export(&["--runs=2", "--counters", "sleep 0.5"]);
    assert_eq!(json["results"][0]["mean_effective_frequency_hz"], 1.0e9);
    assert!(json["results"][0]["counters"].get("task-clock").is_none());
}

#[test]
fn explicit_task_clock_is_a_counter_in_nanoseconds() {
    let json = export(&["--runs=2", "--counters=cycles,task-clock", "sleep 0.5"]);
    let r = &json["results"][0];
    assert_eq!(r["counters"]["task-clock"][0], 500_000_000u64);
    assert_eq!(r["mean_effective_frequency_hz"], 1.0e9);
}

#[test]
fn frequency_line_and_no_flag_means_no_frequency() {
    perfratio()
        .args(["--debug-mode", "--runs=2", "--counters=cycles"])
        .arg("sleep 0.2")
        .assert()
        .success()
        .stdout(predicate::str::contains("cycles 200.0M @1.00 GHz"));
    let json = export(&["--runs=2", "sleep 0.1"]);
    assert!(json["results"][0].get("effective_frequency_hz").is_none());
}

#[test]
fn unknown_name_lists_software_events_and_shorthands() {
    perfratio()
        .args(["--debug-mode", "--counters=migrations", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("cpu-migrations").and(predicate::str::contains(
                "shorthands: default, sw, frequency",
            )),
        );
}

#[test]
fn metric_instructions_ranks_by_the_counter_and_enables_it() {
    // no `--counters`: `--metric` collects its own counter
    let json = export(&[
        "--runs=3",
        "--metric=instructions",
        "sleep 0.5",
        "sleep 0.2",
    ]);
    assert_eq!(json["perfratio"]["measurements"]["counters"], "measured");
    let r = &json["results"][0];
    assert_eq!(r["counters"].as_object().unwrap().len(), 1);
    assert_eq!(r["counters_summary"]["instructions"]["mean"], 1e9);
    perfratio()
        .args([
            "--debug-mode",
            "--runs=3",
            "--metric=instructions",
            "sleep 0.5",
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Instructions (mean ± σ):"))
        .stdout(predicate::str::contains("times fewer instructions than"));
}

#[test]
fn metric_cycles_keeps_an_explicit_counter_list() {
    let json = export(&[
        "--runs=2",
        "--metric=cycles",
        "--counters=instructions",
        "sleep 0.1",
    ]);
    let counters = json["results"][0]["counters"].as_object().unwrap();
    assert_eq!(
        counters.keys().collect::<Vec<_>>(),
        ["cycles", "instructions"]
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn metric_instructions_fails_clearly_without_counters() {
    perfratio()
        .args(["--runs=2", "--metric=instructions", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'--metric instructions' requires a 'instructions' reading for every run",
        ));
}

#[test]
fn raw_events_get_their_own_keys_and_scope_suffix() {
    let json = export(&[
        "--runs=2",
        "--counters=instructions,raw:r01C2,r01c2:u",
        "sleep 0.1",
    ]);
    let r = &json["results"][0];
    let keys: Vec<&String> = r["counters"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["instructions", "r01c2", "r01c2:u"]);
    // 1,000,000 counts/s for 0.1 simulated seconds; user is a quarter
    assert_eq!(r["counters"]["r01c2"][0], 100_000u64);
    assert_eq!(r["counters"]["r01c2:u"][0], 25_000u64);
    assert_eq!(r["counters_summary"]["r01c2"]["mean"], 100_000.0);
}

#[test]
fn bad_raw_event_is_a_usage_error() {
    perfratio()
        .args(["--debug-mode", "--counters=raw:rzz", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a raw event"));
    perfratio()
        .args(["--debug-mode", "--counters=nonexistent/event=1/", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn unknown_name_points_to_list_events() {
    perfratio()
        .args(["--debug-mode", "--counters=bogus", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--list-events"));
}

#[test]
fn list_events_needs_no_command() {
    perfratio()
        .arg("--list-events")
        .assert()
        .success()
        .stdout(predicate::str::contains("Built-in events"))
        .stdout(predicate::str::contains("raw:rXXXX"))
        .stdout(predicate::str::contains("cache-misses"));
}
