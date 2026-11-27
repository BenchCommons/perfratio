//! Collector plugin protocol (#384) and Valgrind adapter (#383).
//!
//! The Valgrind tests run against a fake `valgrind` script that writes a fixture profile, so
//! they work without Valgrind; the test with the real binary is skipped when it is absent.

mod common;
use common::{example_collector, perfratio};

use predicates::prelude::*;
use serde_json::Value;

fn export(command: &mut assert_cmd::Command, args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    command
        .args(args)
        .args(["--export-json", path.to_str().unwrap()])
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn descriptor<'a>(document: &'a Value, id: &str) -> &'a Value {
    document["perfratio"]["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == id)
        .unwrap_or_else(|| panic!("no descriptor {id}"))
}

#[test]
fn plugin_metrics_are_exported_per_run_with_descriptors() {
    let plugin = example_collector();
    let document = export(
        &mut perfratio(),
        &[
            "--runs=3",
            "--warmup=1",
            "-N",
            "--collector-plugin",
            plugin.to_str().unwrap(),
            "sleep 0.05",
        ],
    );
    let result = &document["results"][0];
    let elapsed = result["collector_metrics"]["example.elapsed"]
        .as_array()
        .unwrap();
    assert_eq!(elapsed.len(), 3, "one value per timed run, none for warmup");
    assert!(elapsed.iter().all(|v| v.as_f64().unwrap() >= 0.05));
    assert_eq!(
        result["collector_metrics"]["example.runs"],
        serde_json::json!([1.0, 2.0, 3.0])
    );
    assert!(result["collector_metrics_summary"]["example.elapsed"]["mean"].is_number());

    let d = descriptor(&document, "collector.example.elapsed");
    assert_eq!(d["unit"], "s");
    assert_eq!(d["kind"], "measured");
    assert_eq!(d["scope"], "process");
    assert_eq!(d["source"], "plugin:example");
    assert_eq!(d["status"], "available");
    // declared defaults
    let runs = descriptor(&document, "collector.example.runs");
    assert_eq!(runs["direction"], "neutral");
}

#[test]
fn a_plugin_that_breaks_the_protocol_fails_the_benchmark() {
    let plugin = example_collector();
    for (mode, message) in [
        ("bad_hello", "protocol"),
        ("undeclared", "undeclared metric"),
        ("error", "sensor offline"),
        ("nan", "number or null"),
        ("exit_at_start", "closed its output"),
    ] {
        perfratio()
            .env("EXAMPLE_COLLECTOR_MODE", mode)
            .args(["--runs=2", "-N", "--collector-plugin"])
            .arg(&plugin)
            .arg("true")
            .assert()
            .failure()
            .stderr(predicate::str::contains(message));
    }
}

#[test]
fn a_missing_plugin_is_an_error() {
    perfratio()
        .args([
            "--runs=1",
            "-N",
            "--collector-plugin",
            "/nonexistent/plugin",
            "true",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not start the collector plugin",
        ));
}

#[test]
fn collectors_reject_parallel_runs() {
    let plugin = example_collector();
    perfratio()
        .args(["--runs=1", "-N", "--concurrency=2", "--collector-plugin"])
        .arg(&plugin)
        .arg("true")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--concurrency"));
    perfratio()
        .args([
            "--runs=1",
            "-N",
            "--jobs=2",
            "--collector",
            "cachegrind",
            "true",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--jobs"));
}

#[test]
fn valgrind_is_required_for_the_adapter() {
    perfratio()
        .env("PERFRATIO_VALGRIND", "/nonexistent/valgrind")
        .args(["--runs=1", "-N", "--collector", "cachegrind", "true"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs Valgrind"));
}

#[cfg(unix)]
mod fake_valgrind {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const PROFILE: &str = "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\\n\
        fn=main\\n1 1000 2 1 400 10 5 200 4 2\\nsummary: 1000 2 1 400 10 5 200 4 2\\n";

    /// A script that behaves like Valgrind as far as perfratio can tell: it writes the
    /// profile to `--cachegrind-out-file` (with `%p` replaced) and runs the command.
    fn script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("valgrind");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn fake(dir: &std::path::Path) -> std::path::PathBuf {
        script(
            dir,
            &format!(
                r#"[ "$1" = "--version" ] && echo valgrind-fake && exit 0
out=
for a in "$@"; do
  case "$a" in
    --cachegrind-out-file=*) out="${{a#--cachegrind-out-file=}}" ;;
    --) shift; break ;;
  esac
  shift
done
printf '{PROFILE}' > "$(echo "$out" | sed "s/%p/$$/")"
"$@""#
            ),
        )
    }

    #[test]
    fn a_shell_run_warns_that_the_counts_include_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let valgrind = fake(dir.path());
        for (shell, warns) in [("--shell=default", true), ("--shell=none", false)] {
            let out = perfratio()
                .env("PERFRATIO_VALGRIND", &valgrind)
                .args(["--runs=1", shell, "--collector", "cachegrind", "true"])
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(stderr.contains("runs under a shell"), warns, "{stderr}");
        }
    }

    #[test]
    fn cachegrind_metrics_are_simulated_and_not_timed() {
        let dir = tempfile::tempdir().unwrap();
        let valgrind = fake(dir.path());
        let document = export(
            perfratio().env("PERFRATIO_VALGRIND", &valgrind),
            &["--runs=3", "-N", "--collector", "cachegrind", "true"],
        );
        let metrics = &document["results"][0]["collector_metrics"];
        assert_eq!(
            metrics["cachegrind.instructions"],
            serde_json::json!([1000.0])
        );
        assert_eq!(
            metrics["cachegrind.l1_data_misses"],
            serde_json::json!([14.0])
        );
        assert_eq!(document["results"][0]["times"].as_array().unwrap().len(), 3);

        let d = descriptor(&document, "collector.cachegrind.instructions");
        assert_eq!(d["kind"], "simulated");
        assert_eq!(d["source"], "valgrind.cachegrind");
        assert_eq!(d["unit"], "count");
        assert_eq!(d["direction"], "lower_is_better");
        let cycles = descriptor(&document, "collector.cachegrind.estimated_cycles");
        assert_eq!(cycles["kind"], "estimated");
    }

    #[test]
    fn the_command_failing_under_valgrind_is_an_error_unless_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let valgrind = fake(dir.path());
        perfratio()
            .env("PERFRATIO_VALGRIND", &valgrind)
            .args(["--runs=1", "-N", "-i", "--collector", "callgrind", "false"])
            .assert()
            // the fake only writes cachegrind files, so callgrind finds no profile
            .failure()
            .stderr(predicate::str::contains("wrote no profile"));
    }

    #[test]
    fn a_failing_command_under_valgrind_fails_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let valgrind = script(
            dir.path(),
            r#"[ "$1" = "--version" ] && exit 0
exit 3"#,
        );
        perfratio()
            .env("PERFRATIO_VALGRIND", &valgrind)
            .args(["--runs=1", "-N", "--collector", "cachegrind", "true"])
            .assert()
            .failure();
    }
}

fn real_valgrind() -> bool {
    std::process::Command::new("valgrind")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn real_cachegrind_counts_are_deterministic() {
    if !real_valgrind() {
        eprintln!("skipped: valgrind is not installed");
        return;
    }
    let run = || {
        export(
            &mut perfratio(),
            &["--runs=1", "-N", "--collector", "cachegrind", "true"],
        )["results"][0]["collector_metrics"]["cachegrind.instructions"][0]
            .as_f64()
            .unwrap()
    };
    let (first, second) = (run(), run());
    assert!(first > 0.0);
    assert!((first - second).abs() / first < 0.01, "{first} vs {second}");
}
