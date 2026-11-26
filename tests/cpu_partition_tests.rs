//! Tests for --pin-workers and --reserve-cpus (#307). The partition function
//! itself is covered by unit tests; these check the options, the JSON export
//! and, on Linux, the affinity the workers really get.

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

#[allow(dead_code)]
fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

#[test]
fn the_options_need_jobs() {
    for args in [
        &["--pin-workers"][..],
        &["--pin-workers=none"],
        &["--reserve-cpus", "1"],
    ] {
        perfratio()
            .args(["--debug-mode", "--runs=2"])
            .args(args)
            .arg("sleep 0.1")
            .assert()
            .failure()
            .stderr(predicate::str::contains("need '--jobs' > 1"));
    }
}

#[test]
fn reserving_cpus_needs_pinned_workers() {
    perfratio()
        .args([
            "--debug-mode",
            "-j2",
            "--pin-workers=none",
            "--reserve-cpus=1",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pin-workers=none"));
}

#[test]
fn the_reserve_value_is_validated() {
    for invalid in ["0", "x", "3-1", ""] {
        perfratio()
            .args([
                "--debug-mode",
                "-j2",
                "--reserve-cpus",
                invalid,
                "sleep 0.1",
            ])
            .assert()
            .failure();
    }
}

#[test]
fn unpinned_workers_have_no_partition_in_the_json() {
    let json = export(&[
        "--debug-mode",
        "--runs=3",
        "-j2",
        "--pin-workers=none",
        "sleep 0.1",
    ]);
    assert!(json["perfratio"]["execution"]
        .get("cpu_partition")
        .is_none());
    // the default in debug mode starts no processes, so nothing is pinned
    let json = export(&["--debug-mode", "--runs=3", "-j2", "sleep 0.1"]);
    assert!(json["perfratio"]["execution"]
        .get("cpu_partition")
        .is_none());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn the_partition_is_exported_with_disjoint_worker_cpus() {
    if cpus() < 2 {
        return;
    }
    let json = export(&[
        "--debug-mode",
        "--runs=3",
        "-j2",
        "--pin-workers",
        "sleep 0.1",
    ]);
    let partition = &json["perfratio"]["execution"]["cpu_partition"];
    let workers = partition["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 2);
    let all: Vec<u64> = workers
        .iter()
        .flat_map(|w| w.as_array().unwrap().iter().map(|c| c.as_u64().unwrap()))
        .collect();
    let unique: std::collections::BTreeSet<_> = all.iter().collect();
    assert_eq!(unique.len(), all.len(), "worker CPUs overlap: {all:?}");
    assert!(partition.get("reserved").is_none());
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn reserved_cpus_are_exported_and_not_given_to_workers() {
    if cpus() < 3 {
        return;
    }
    let json = export(&[
        "--debug-mode",
        "--runs=3",
        "-j2",
        "--reserve-cpus=1",
        "sleep 0.1",
    ]);
    let partition = &json["perfratio"]["execution"]["cpu_partition"];
    let reserved = partition["reserved"].as_array().unwrap();
    assert!(!reserved.is_empty());
    for worker in partition["workers"].as_array().unwrap() {
        for cpu in worker.as_array().unwrap() {
            assert!(!reserved.contains(cpu), "{partition}");
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn more_jobs_than_usable_cpus_is_an_error_with_pinned_workers() {
    perfratio()
        .args([
            "--debug-mode",
            "--jobs=100000",
            "--pin-workers",
            "sleep 0.1",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "'--jobs 100000' needs 100000 CPUs",
        ));
    // without pinning it is only a warning
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "--jobs=100000",
            "--pin-workers=none",
            "sleep 0.1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("exceeds"));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_warns_and_does_not_pin() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .args([
            "--debug-mode",
            "--runs=2",
            "-j2",
            "--pin-workers",
            "sleep 0.1",
        ])
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success()
        .stderr(predicate::str::contains("cannot be pinned"));
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(json["perfratio"]["execution"]
        .get("cpu_partition")
        .is_none());
}

#[cfg(target_os = "linux")]
fn parse_cpu_list(list: &str) -> Vec<u64> {
    list.split(',')
        .flat_map(|part| match part.split_once('-') {
            Some((a, b)) => (a.parse::<u64>().unwrap()..=b.parse().unwrap()).collect(),
            None => vec![part.parse().unwrap()],
        })
        .collect()
}

#[cfg(target_os = "linux")]
#[test]
fn workers_run_on_their_own_cpus_and_never_on_reserved_ones() {
    if cpus() < 3 {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("affinity");
    let json_path = dir.path().join("out.json");
    // iteration 0 is the sequential initial measurement and is not pinned
    let command = format!(
        "echo $PERFRATIO_ITERATION $PERFRATIO_WORKER \
         $(grep Cpus_allowed_list /proc/self/status) >> {}; sleep 0.05",
        log.display()
    );
    perfratio()
        .args([
            "--runs=8",
            "-j2",
            "--reserve-cpus=1",
            "--style=basic",
            &command,
        ])
        .arg("--export-json")
        .arg(&json_path)
        .assert()
        .success();
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    let partition = &json["perfratio"]["execution"]["cpu_partition"];
    let sets: Vec<Vec<u64>> = partition["workers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            let mut cpus: Vec<u64> = w
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_u64().unwrap())
                .collect();
            cpus.sort_unstable();
            cpus
        })
        .collect();
    let reserved: Vec<u64> = partition["reserved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_u64().unwrap())
        .collect();

    let text = std::fs::read_to_string(&log).unwrap();
    let mut pinned = 0;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // "<iteration> <worker> Cpus_allowed_list: <list>"
        if fields.first() == Some(&"0") {
            continue;
        }
        assert_eq!(fields.len(), 4, "{line}");
        let slot: usize = fields[1].parse().unwrap();
        let mut allowed = parse_cpu_list(fields[3]);
        allowed.sort_unstable();
        assert_eq!(allowed, sets[slot], "worker {slot}: {line}");
        assert!(allowed.iter().all(|c| !reserved.contains(c)), "{line}");
        pinned += 1;
    }
    assert_eq!(pinned, 7, "{text}");
}
