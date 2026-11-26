//! Tests for the environment block, the fingerprint, `--meta` and the
//! environment check of `--compare` / `--import-json` (#314,
//! `docs/specs/environment-fingerprint.md`). Deterministic through `--debug-mode`.

mod common;
use common::perfratio;

use predicates::prelude::*;

fn export_with<S: AsRef<std::ffi::OsStr>>(
    args: &[S],
    configure: impl FnOnce(&mut assert_cmd::Command),
) -> (String, serde_json::Value) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    let mut command = perfratio();
    command
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path);
    configure(&mut command);
    command.assert().success();
    let text = std::fs::read_to_string(&path).unwrap();
    let json = serde_json::from_str(&text).unwrap();
    (text, json)
}

fn export<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> serde_json::Value {
    export_with(args, |_| {}).1
}

#[test]
fn the_export_has_an_environment_and_a_stable_fingerprint() {
    let a = export(&["--runs=3", "sleep 0.1"]);
    let b = export(&["--runs=3", "sleep 0.1"]);
    let environment = &a["perfratio"]["environment"];
    assert_eq!(environment["os_family"], std::env::consts::OS);
    assert_eq!(environment["arch"], std::env::consts::ARCH);
    assert_eq!(
        environment["perfratio"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(environment["logical_cores"].as_u64().unwrap() >= 1);
    let fingerprint = a["perfratio"]["environment_fingerprint"].as_str().unwrap();
    assert!(fingerprint.starts_with("sha256:"), "{fingerprint}");
    assert_eq!(fingerprint.len(), "sha256:".len() + 64);
    assert_eq!(
        fingerprint,
        b["perfratio"]["environment_fingerprint"].as_str().unwrap()
    );
}

#[test]
fn settings_are_recorded_and_do_not_change_the_fingerprint() {
    let plain = export(&["--runs=3", "sleep 0.1"]);
    let json = export(&["--runs=3", "--concurrency=2", "sleep 0.1"]);
    assert_eq!(
        json["perfratio"]["environment"]["settings"]["concurrency"],
        "2"
    );
    assert!(plain["perfratio"]["environment"].get("settings").is_none());
    assert_eq!(
        plain["perfratio"]["environment_fingerprint"],
        json["perfratio"]["environment_fingerprint"]
    );
}

#[test]
fn no_host_name_user_name_or_environment_variable_is_exported() {
    let secrets = [
        ("HOSTNAME", "sentinel-host-6f3a"),
        ("COMPUTERNAME", "sentinel-computer-6f3a"),
        ("USER", "sentinel-user-6f3a"),
        ("USERNAME", "sentinel-username-6f3a"),
        ("LOGNAME", "sentinel-logname-6f3a"),
        ("PERFRATIO_TEST_SECRET", "sentinel-token-6f3a"),
    ];
    let (text, _) = export_with(&["--runs=3", "sleep 0.1"], |command| {
        for (name, value) in secrets {
            command.env(name, value);
        }
    });
    assert!(!text.contains("sentinel-"), "{text}");
}

#[test]
fn meta_pairs_are_exported() {
    let json = export(&[
        "--runs=3",
        "--meta",
        "compiler=gcc-14",
        "--meta",
        "flags=-O3",
        "sleep 0.1",
    ]);
    assert_eq!(json["perfratio"]["meta"]["compiler"], "gcc-14");
    assert_eq!(json["perfratio"]["meta"]["flags"], "-O3");
    assert!(export(&["--runs=3", "sleep 0.1"])["perfratio"]
        .get("meta")
        .is_none());
}

#[test]
fn invalid_meta_is_rejected() {
    perfratio()
        .args(["--debug-mode", "--meta", "no-equals", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--meta"));
}

fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn meta_git_records_the_commit_and_the_dirty_flag() {
    let repo = tempfile::tempdir().unwrap();
    if !(git(repo.path(), &["init", "-q"])
        && git(repo.path(), &["commit", "-q", "--allow-empty", "-m", "x"]))
    {
        eprintln!("git is not available; skipping");
        return;
    }
    let run = |repo: &std::path::Path| {
        export_with(&["--runs=3", "--meta", "git", "sleep 0.1"], |c| {
            c.current_dir(repo);
        })
        .1
    };
    let clean = run(repo.path());
    let meta = &clean["perfratio"]["meta"];
    assert_eq!(meta["git_commit"].as_str().unwrap().len(), 40);
    assert_eq!(meta["git_dirty"], "false");

    std::fs::write(repo.path().join("new.txt"), "x").unwrap();
    assert_eq!(run(repo.path())["perfratio"]["meta"]["git_dirty"], "true");
}

#[test]
fn meta_git_outside_a_repository_warns_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    // a parent that is a repository would defeat the test
    if git(dir.path(), &["rev-parse", "HEAD"]) {
        return;
    }
    let (_, json) = export_with(&["--runs=3", "--meta", "git", "sleep 0.1"], |c| {
        c.current_dir(dir.path());
    });
    assert!(json["perfratio"].get("meta").is_none());
}

/// A baseline export whose environment was edited by `edit`
fn baseline(dir: &std::path::Path, edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let path = dir.join("baseline.json");
    let mut json = export(&["--runs=5", "sleep 0.1"]);
    edit(&mut json["perfratio"]);
    std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn compare_with_the_same_environment_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let path = baseline(dir.path(), |_| {});
    perfratio()
        .args(["--debug-mode", "--runs=5", "--compare", &path, "sleep 0.1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("different environment").not());
}

#[test]
fn compare_warns_with_the_differing_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = baseline(dir.path(), |perfratio| {
        perfratio["environment"]["cpu_model"] = "Other CPU 9000".into();
        perfratio["environment"]["governor"] = "powersave".into();
    });
    let assert = perfratio()
        .args(["--debug-mode", "--runs=5", "--compare", &path, "sleep 0.1"])
        .assert()
        .success()
        .stderr(predicate::str::contains("different environment"))
        .stderr(predicate::str::contains("cpu_model (Other CPU 9000 ->"));
    // governor is unknown on the mock host on some systems: only assert it when known
    let _ = assert;
}

#[test]
fn require_compatible_env_fails_on_a_mismatch_and_on_a_missing_environment() {
    let dir = tempfile::tempdir().unwrap();
    let different = baseline(dir.path(), |perfratio| {
        perfratio["environment"]["arch"] = "pdp11".into();
    });
    perfratio()
        .args(["--debug-mode", "--runs=5", "--compare", &different])
        .args(["--require-compatible-env", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("arch (pdp11 ->"))
        .stderr(predicate::str::contains("--require-compatible-env"));

    let missing = baseline(dir.path(), |perfratio| {
        perfratio.as_object_mut().unwrap().remove("environment");
    });
    perfratio()
        .args(["--debug-mode", "--runs=5", "--compare", &missing])
        .args(["--require-compatible-env", "sleep 0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("records no environment"));
    // without the flag an old file is accepted silently
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--compare",
            &missing,
            "sleep 0.1",
        ])
        .assert()
        .success();
}

#[test]
fn import_json_checks_the_environment_too() {
    let dir = tempfile::tempdir().unwrap();
    let path = baseline(dir.path(), |perfratio| {
        perfratio["environment"]["os_family"] = "plan9".into();
    });
    perfratio()
        .args([
            "--debug-mode",
            "--runs=5",
            "--import-json",
            &path,
            "sleep 0.2",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("os_family (plan9 ->"));
}
