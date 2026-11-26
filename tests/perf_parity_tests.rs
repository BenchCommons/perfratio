//! perf-stat parity harness for `--counters` (#312), `scripts/perf_parity.py`.
//!
//! `the_harness_logic_is_correct` runs everywhere Python 3 is installed. The real comparison
//! needs Linux, `perf` and a readable PMU (bare metal or a self-hosted runner), so it is
//! `#[ignore]`d: `cargo test --locked --test perf_parity_tests -- --ignored`.
//! See `docs/specs/perf-parity-harness.md`.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/perf_parity.py")
}

fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| {
        Command::new(p)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

#[test]
fn the_harness_logic_is_correct() {
    let Some(python) = python() else {
        eprintln!("python is not installed, skipping");
        return;
    };
    let output = Command::new(python)
        .arg(script())
        .arg("--self-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A host without perf or a PMU exits 77 ("skipped"). That is a failure here unless
/// `PERFRATIO_PARITY_ALLOW_SKIP` is set, so an explicit `--ignored` run cannot pass silently.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "needs perf and a readable PMU (bare-metal Linux)"]
fn counters_match_perf_stat_and_count_the_child_tree() {
    let python = python().expect("python 3 is needed for the harness");
    let mut command = Command::new(python);
    command
        .arg(script())
        .arg("--perfratio")
        .arg(assert_cmd::cargo_bin!("perfratio"));
    if let Ok(cpu) = std::env::var("PERFRATIO_PARITY_PIN") {
        command.args(["--pin", &cpu]);
    }
    let output = command.output().unwrap();
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match output.status.code() {
        Some(0) => println!("{report}"),
        Some(77) if std::env::var_os("PERFRATIO_PARITY_ALLOW_SKIP").is_some() => {
            println!("SKIPPED, not passed:\n{report}")
        }
        _ => panic!("perf parity harness did not pass:\n{report}"),
    }
}
