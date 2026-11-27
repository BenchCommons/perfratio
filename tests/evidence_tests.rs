//! `--evidence DIR` and `--verify-evidence DIR` (#375): round trip and tamper tests.

mod common;
use common::perfratio;

use serde_json::Value;
use std::fs;
use std::path::Path;

fn bundle(dir: &Path, extra: &[&str]) {
    perfratio()
        .arg("--debug-mode")
        .args(["--runs", "3"])
        .arg("--evidence")
        .arg(dir)
        .args(extra)
        .args(["sleep 0.1", "sleep 0.2"])
        .assert()
        .success();
}

fn verify(dir: &Path) -> assert_cmd::assert::Assert {
    perfratio().arg("--verify-evidence").arg(dir).assert()
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn fresh() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bundle");
    bundle(&dir, &[]);
    (tmp, dir)
}

fn failure_text(dir: &Path) -> String {
    let out = verify(dir).failure().code(1).get_output().clone();
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn round_trip_writes_every_file_and_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bundle");
    let export = tmp.path().join("out.json");
    bundle(&dir, &["--export-json", export.to_str().unwrap()]);

    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "calibration.json",
            "command.json",
            "environment.json",
            "manifest.json",
            "quality.json",
            "results.json",
            "samples.jsonl",
            "validation.json"
        ]
    );

    // results.json is the JSON export itself
    assert_eq!(
        fs::read(dir.join("results.json")).unwrap(),
        fs::read(&export).unwrap()
    );

    let samples = fs::read_to_string(dir.join("samples.jsonl")).unwrap();
    let lines: Vec<Value> = samples
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[3]["command_index"], 1);
    assert_eq!(lines[3]["run"], 0);
    assert!(lines[3]["time"].as_f64().unwrap() > 0.19);

    let environment = json(&dir.join("environment.json"));
    assert!(environment["environment"]["os_family"].is_string());
    assert!(environment["environment_fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let command = json(&dir.join("command.json"));
    assert_eq!(command["argv"][0], "perfratio");
    assert!(command["argv"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "sleep 0.2"));
    assert_eq!(json(&dir.join("calibration.json"))["available"], false);
    assert_eq!(json(&dir.join("quality.json"))["available"], true);

    let manifest = json(&dir.join("manifest.json"));
    assert_eq!(manifest["format"], "perfratio-evidence");
    assert_eq!(manifest["format_version"], "1.0");
    assert_eq!(
        manifest["results_schema_version"],
        json(&dir.join("results.json"))["schemaVersion"]
    );

    verify(&dir)
        .success()
        .stdout(predicates::str::contains("ok       results.json"))
        .stdout(predicates::str::contains("manifest sha256: "));
}

#[test]
fn manifest_matches_the_published_schema() {
    let (_tmp, dir) = fresh();
    let schema = json(Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/schema/perfratio-evidence-manifest.schema.json"
    )));
    let validator = jsonschema::validator_for(&schema).unwrap();
    let manifest = json(&dir.join("manifest.json"));
    let errors: Vec<String> = validator
        .iter_errors(&manifest)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}");

    // the schema rejects a manifest without a required file or with a path
    let mut broken = manifest.clone();
    broken["files"].as_array_mut().unwrap().pop();
    assert!(!validator.is_valid(&broken));
    let mut broken = manifest;
    broken["files"][0]["path"] = "../x".into();
    assert!(!validator.is_valid(&broken));
}

#[test]
fn tampered_file_is_detected() {
    let (_tmp, dir) = fresh();
    let path = dir.join("results.json");
    let mut text = fs::read_to_string(&path).unwrap();
    text = text.replacen("\"mean\"", "\"mean \"", 1);
    fs::write(&path, text).unwrap();
    let stderr = failure_text(&dir);
    assert!(stderr.contains("'results.json'"), "{stderr}");
}

#[test]
fn same_size_edit_is_detected_by_the_hash() {
    let (_tmp, dir) = fresh();
    let path = dir.join("samples.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    let i = bytes.iter().position(|b| *b == b'1').unwrap();
    bytes[i] = b'2';
    fs::write(&path, bytes).unwrap();
    assert!(failure_text(&dir).contains("does not match its sha256"));
}

#[test]
fn missing_file_and_extra_file_are_detected() {
    let (_tmp, dir) = fresh();
    fs::write(dir.join("notes.txt"), "hello").unwrap();
    assert!(failure_text(&dir).contains("'notes.txt' is not listed"));
    fs::remove_file(dir.join("notes.txt")).unwrap();
    fs::remove_file(dir.join("quality.json")).unwrap();
    assert!(failure_text(&dir).contains("'quality.json' is missing"));
}

#[test]
fn damaged_manifests_are_rejected() {
    let (_tmp, dir) = fresh();
    let manifest_path = dir.join("manifest.json");
    let original = json(&manifest_path);
    let write = |value: &Value| fs::write(&manifest_path, serde_json::to_vec(value).unwrap());

    let mut bad = original.clone();
    bad["format"] = "other".into();
    write(&bad).unwrap();
    assert!(failure_text(&dir).contains("format is 'other'"));

    let mut bad = original.clone();
    bad["format_version"] = "2.0".into();
    write(&bad).unwrap();
    assert!(failure_text(&dir).contains("unsupported format_version"));

    // a newer MINOR of the same MAJOR is read
    let mut newer = original.clone();
    newer["format_version"] = "1.7".into();
    newer["future_key"] = true.into();
    write(&newer).unwrap();
    verify(&dir).success();

    let mut bad = original.clone();
    bad["files"][0]["path"] = "../manifest.json".into();
    write(&bad).unwrap();
    assert!(failure_text(&dir).contains("invalid file name"));

    let mut bad = original.clone();
    let first = bad["files"][0].clone();
    bad["files"].as_array_mut().unwrap().push(first);
    write(&bad).unwrap();
    assert!(failure_text(&dir).contains("is listed twice"));

    let mut bad = original;
    bad["files"].as_array_mut().unwrap().remove(0);
    write(&bad).unwrap();
    assert!(failure_text(&dir).contains("does not list the required"));

    fs::write(&manifest_path, "not json").unwrap();
    assert!(failure_text(&dir).contains("manifest.json is invalid"));
    fs::remove_file(&manifest_path).unwrap();
    assert!(failure_text(&dir).contains("cannot read"));
}

#[test]
fn manifest_hash_is_printed_and_stable() {
    let (_tmp, dir) = fresh();
    let first = String::from_utf8(verify(&dir).success().get_output().stdout.clone()).unwrap();
    let pinned = first.lines().last().unwrap().to_string();
    assert!(pinned.starts_with("manifest sha256: "));
    let again = String::from_utf8(verify(&dir).success().get_output().stdout.clone()).unwrap();
    assert_eq!(again.lines().last().unwrap(), pinned);
}

#[test]
fn a_non_empty_directory_is_refused_before_benchmarking() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("keep.txt"), "x").unwrap();
    perfratio()
        .arg("--debug-mode")
        .arg("--evidence")
        .arg(tmp.path())
        .arg("sleep 0.1")
        .assert()
        .failure()
        .stderr(predicates::str::contains("is not empty"));
    assert_eq!(
        fs::read_to_string(tmp.path().join("keep.txt")).unwrap(),
        "x"
    );
}

#[test]
fn an_existing_empty_directory_is_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    bundle(tmp.path(), &[]);
    verify(tmp.path()).success();
}

#[test]
fn bundle_has_no_host_name_or_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bundle");
    perfratio()
        .env("PERFRATIO_TEST_SECRET", "s3cr3t-value-123")
        .arg("--debug-mode")
        .args(["--runs", "3", "--evidence"])
        .arg(&dir)
        .arg("sleep 0.1")
        .assert()
        .success();
    let host = std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| h.len() > 3);
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("s3cr3t-value-123"), "{}", path.display());
        if let Some(host) = &host {
            assert!(!text.contains(host.as_str()), "{}", path.display());
        }
    }
}

#[test]
fn verify_evidence_does_not_take_commands() {
    let (_tmp, dir) = fresh();
    perfratio()
        .arg("--verify-evidence")
        .arg(&dir)
        .arg("sleep 1")
        .assert()
        .failure();
}
