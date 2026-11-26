//! Tests for the unique run identity (#308): `PERFRATIO_RUN_ID`,
//! `PERFRATIO_WORKER_ID`, `--artifact-dir` and the `run_ids` JSON fields.

mod common;
use common::perfratio;

use serde_json::Value;
use std::collections::BTreeSet;

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

fn ids(value: &Value) -> Vec<u64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect()
}

/// Shell command line that appends `text` (which may use `PERFRATIO_*`
/// variables, written `{RUN_ID}` and `{WORKER_ID}`) to `file`.
fn append(text: &str, file: &std::path::Path) -> String {
    let file = file.display();
    if cfg!(windows) {
        let text = text
            .replace("{RUN_ID}", "%PERFRATIO_RUN_ID%")
            .replace("{WORKER_ID}", "%PERFRATIO_WORKER_ID%");
        // the space keeps a one-digit id from reading as a redirected handle
        format!("echo {text} >> \"{file}\"")
    } else {
        let text = text
            .replace("{RUN_ID}", "$PERFRATIO_RUN_ID")
            .replace("{WORKER_ID}", "$PERFRATIO_WORKER_ID");
        format!("echo {text} >> '{file}'")
    }
}

fn lines(file: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn prepare_command_and_conclude_of_one_run_see_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("log");
    perfratio()
        .args(["--warmup=1", "--runs=3", "--style=none"])
        .arg("--prepare")
        .arg(append("prepare {RUN_ID}", &file))
        .arg("--conclude")
        .arg(append("conclude {RUN_ID}", &file))
        .arg(append("run {RUN_ID}", &file))
        .assert()
        .success();

    let mut by_id = std::collections::BTreeMap::<u64, Vec<String>>::new();
    for line in lines(&file) {
        let (what, id) = line.split_once(' ').unwrap();
        by_id
            .entry(id.parse().unwrap())
            .or_default()
            .push(what.to_string());
    }
    // 1 warmup + 3 timed runs: ids 0..4, each seen by all three hooks
    assert_eq!(by_id.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2, 3]);
    for (id, mut what) in by_id {
        what.sort();
        assert_eq!(what, ["conclude", "prepare", "run"], "run {id}");
    }
}

// Appending to one file from several processes at once can hit a sharing
// violation on Windows
#[cfg(unix)]
#[test]
fn concurrent_workers_never_share_a_run_id() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("log");
    let json = export(&[
        "--warmup=1",
        "--runs=3",
        "--concurrency=4",
        "--style=none",
        &append("{RUN_ID} {WORKER_ID}", &file),
    ]);

    let seen: Vec<(u64, u64)> = lines(&file)
        .iter()
        .map(|line| {
            let (id, worker) = line.split_once(' ').unwrap();
            (id.parse().unwrap(), worker.parse().unwrap())
        })
        .collect();
    // (1 warmup + 3 runs) x 4 instances: ids 0..16, no duplicates
    assert_eq!(seen.len(), 16);
    let unique: BTreeSet<u64> = seen.iter().map(|(id, _)| *id).collect();
    assert_eq!(unique, (0..16).collect());
    // the worker id is the position in the batch
    for (id, worker) in &seen {
        assert_eq!(id % 4, *worker, "run id {id}");
    }

    let result = &json["results"][0];
    assert_eq!(ids(&result["run_ids"]), vec![4, 8, 12]);
    let rows: Vec<Vec<u64>> = result["instance_run_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(ids)
        .collect();
    assert_eq!(
        rows,
        vec![vec![4, 5, 6, 7], vec![8, 9, 10, 11], vec![12, 13, 14, 15]]
    );
}

#[test]
fn run_ids_are_exported_and_continue_across_commands() {
    let json = export(&[
        "--debug-mode",
        "--warmup=1",
        "--runs=3",
        "sleep 0.1",
        "sleep 0.2",
    ]);
    let first = ids(&json["results"][0]["run_ids"]);
    let second = ids(&json["results"][1]["run_ids"]);
    assert_eq!(first, vec![1, 2, 3]);
    assert_eq!(second, vec![5, 6, 7]);
    assert!(json["results"][0].get("instance_run_ids").is_none());
}

#[test]
fn debug_mode_batches_get_consecutive_ids() {
    let json = export(&["--debug-mode", "--runs=2", "--concurrency=3", "sleep 0.1"]);
    let result = &json["results"][0];
    let rows: Vec<Vec<u64>> = result["instance_run_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(ids)
        .collect();
    assert_eq!(rows, vec![vec![0, 1, 2], vec![3, 4, 5]]);
    assert_eq!(ids(&result["run_ids"]), vec![0, 3]);
}

#[test]
fn artifact_dir_gives_every_run_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let artifacts = dir.path().join("artifacts");
    let marker = if cfg!(windows) {
        "echo x > \"%PERFRATIO_RUN_DIR%\\out\""
    } else {
        "echo x > \"$PERFRATIO_RUN_DIR/out\""
    };
    perfratio()
        .args(["--runs=2", "--style=none", "--artifact-dir"])
        .arg(&artifacts)
        .arg(marker)
        .assert()
        .success();
    for id in 0..2 {
        assert!(
            artifacts.join(format!("run-{id}")).join("out").exists(),
            "run-{id}"
        );
    }
}

#[test]
fn energy_of_a_batch_is_not_divided_per_instance() {
    let json = export(&[
        "--debug-mode",
        "--energy",
        "--runs=2",
        "--concurrency=3",
        "sleep 0.1",
    ]);
    let result = &json["results"][0];
    assert!(result["mean_energy_joules"].is_number());
    assert_eq!(result["energy_scope"], "batch");
    assert!(result.get("energy_per_instance_joules").is_none());

    let single = export(&["--debug-mode", "--energy", "--runs=2", "sleep 0.1"]);
    assert_eq!(single["results"][0]["energy_scope"], "system");
}

#[test]
fn a_plain_command_leaves_no_processes_behind() {
    let json = export(&["--runs=3", "--style=none", "echo hi"]);
    assert!(json["results"][0].get("leftover_processes").is_none());
}
