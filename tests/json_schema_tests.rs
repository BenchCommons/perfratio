//! Validates `--export-json` output against `docs/schema/perfratio-export.schema.json` (#240),
//! so that the schema file cannot drift from the exporter.
//!
//! The published schema allows unknown keys, as the contract requires of consumers. The test
//! validates against a strict copy (`additionalProperties: false` on every object that lists
//! its `properties`), so a field the exporter writes but the schema does not describe fails.

mod common;
use common::perfratio;

use serde_json::{json, Value};

const SCHEMA_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/docs/schema/perfratio-export.schema.json"
);

fn schema() -> Value {
    serde_json::from_str(&std::fs::read_to_string(SCHEMA_PATH).unwrap()).unwrap()
}

/// Sets `additionalProperties: false` on every object schema that declares `properties`
/// and does not say anything about additional properties.
fn make_strict(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if map.contains_key("properties") && !map.contains_key("additionalProperties") {
                map.insert("additionalProperties".into(), json!(false));
            }
            map.values_mut().for_each(make_strict);
        }
        Value::Array(items) => items.iter_mut().for_each(make_strict),
        _ => {}
    }
}

fn validator(strict: bool) -> jsonschema::Validator {
    let mut schema = schema();
    if strict {
        make_strict(&mut schema);
    }
    jsonschema::validator_for(&schema).expect("the schema must be a valid 2020-12 schema")
}

fn assert_valid(case: &str, document: &Value) {
    let strict = validator(true);
    let errors: Vec<String> = strict
        .iter_errors(document)
        .map(|e| format!("{} (at {})", e, e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "case `{case}` does not match the schema:\n{}\n\ndocument:\n{document:#}",
        errors.join("\n")
    );
    // the published (non-strict) schema accepts everything the strict one does
    assert!(validator(false).is_valid(document), "case `{case}`");
}

fn export(args: &[&str]) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .assert()
        .success();
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Flag combinations covering every optional part of the export. The mock executor
/// understands `sleep <seconds> [user] [system] [memory]`.
const CASES: &[(&str, &[&str])] = &[
    ("default", &["--runs=3", "sleep 0.1"]),
    ("single run", &["--runs=1", "sleep 0.1"]),
    ("several commands", &["--runs=3", "sleep 0.1", "sleep 0.2"]),
    ("labels", &["--runs=2", "--label", "lang=rust", "sleep 0.1"]),
    (
        "deep stats and many runs",
        &["--runs=40", "--deep-stats", "sleep 0.1"],
    ),
    (
        "adaptive warmup",
        &["--runs=3", "--warmup=auto", "sleep 0.1"],
    ),
    ("fixed warmup", &["--runs=3", "--warmup=2", "sleep 0.1"]),
    (
        "first run separate",
        &["--runs=3", "--first-run=separate", "sleep 0.1"],
    ),
    (
        "first run separate with warmup",
        &[
            "--runs=3",
            "--first-run=separate",
            "--warmup=2",
            "sleep 0.1",
        ],
    ),
    (
        "parameter scan",
        &["--runs=2", "--parameter-scan", "n", "1", "3", "sleep 0.{n}"],
    ),
    (
        "parameter list",
        &["--runs=2", "-L", "n", "1,2", "sleep 0.{n}"],
    ),
    (
        "per-run parameters",
        &[
            "--runs=4",
            "--aggregate-parameter-runs",
            "-L",
            "n",
            "1,2",
            "sleep 0.{n}",
        ],
    ),
    ("energy", &["--runs=3", "--energy", "sleep 0.1"]),
    (
        "energy domains",
        &["--runs=3", "--energy", "--energy-domains", "sleep 0.1"],
    ),
    (
        "energy baseline",
        &["--runs=3", "--energy", "--energy-baseline=1s", "sleep 0.1"],
    ),
    (
        "metric energy",
        &[
            "--runs=3",
            "--energy",
            "--metric=energy",
            "sleep 0.1",
            "sleep 0.2",
        ],
    ),
    (
        "pareto",
        &[
            "--runs=3",
            "--pareto=wall,memory",
            "--constraint=wall<=10s",
            "--objective=minimize:wall",
            "sleep 0.1",
            "sleep 0.2",
        ],
    ),
    (
        "metric memory",
        &["--runs=3", "--metric=memory", "sleep 0.1", "sleep 0.2"],
    ),
    (
        "metric user",
        &["--runs=3", "--metric=user", "sleep 0.1 0.05 0.01"],
    ),
    (
        "metric cpu",
        &["--runs=3", "--metric=cpu", "sleep 0.1 0.05 0.01"],
    ),
    (
        "resource usage",
        &["--runs=3", "--resource-usage", "sleep 0.1"],
    ),
    ("counters", &["--runs=3", "--counters", "sleep 0.5"]),
    ("topdown", &["--runs=3", "--topdown", "sleep 0.5"]),
    (
        "topdown with counters",
        &["--runs=3", "--counters=cycles", "--topdown", "sleep 0.5"],
    ),
    ("simd", &["--runs=3", "--simd", "sleep 0.5"]),
    (
        "simd with counters and user scope",
        &[
            "--runs=3",
            "--counters",
            "--simd",
            "--scope=user",
            "sleep 0.5",
        ],
    ),
    (
        "counters with scope split",
        &["--runs=3", "--counters", "--scope", "split", "sleep 0.5"],
    ),
    (
        "counters with suffixes",
        &[
            "--runs=3",
            "--counters=instructions:u,cycles:k",
            "sleep 0.5",
        ],
    ),
    (
        "metric instructions",
        &[
            "--runs=3",
            "--counters",
            "--metric=instructions",
            "sleep 0.5",
        ],
    ),
    (
        "concurrency",
        &["--runs=2", "--concurrency", "3", "sleep 0.1"],
    ),
    (
        "concurrency with energy",
        &["--runs=2", "--concurrency", "3", "--energy", "sleep 0.1"],
    ),
    ("jobs", &["--runs=4", "--jobs", "2", "sleep 0.1"]),
    (
        "jobs with an exclusive policy",
        &[
            "--runs=4",
            "--jobs=2",
            "--schedule=exclusive",
            "sleep 0.1",
            "sleep 0.05",
        ],
    ),
    (
        "optimize",
        &["--runs=3", "--optimize", "n", "1", "40", "sleep {n}"],
    ),
    (
        "optimize with a fixed parameter",
        &[
            "--runs=3",
            "-L",
            "m",
            "1,2",
            "--optimize",
            "n",
            "1",
            "40",
            "sleep {n}",
        ],
    ),
    (
        "validation omit",
        &[
            "--runs=3",
            "--on-invalid=omit",
            "--expect-output-sha256",
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03",
            "-L",
            "w",
            "hello,bad,hello",
            "--aggregate-parameter-runs",
            "sleep 0.1 0 0 0 {w}",
        ],
    ),
    ("duration", &["--duration=1s", "sleep 0.5"]),
    ("target precision", &["--target-precision=1%", "sleep 0.1"]),
    (
        "discard outliers",
        &["--runs=20", "--discard-outliers", "sleep 0.1"],
    ),
    (
        "outlier method iqr",
        &["--runs=20", "--outlier-method=iqr", "sleep 0.1"],
    ),
    (
        "subtract baseline",
        &["--runs=3", "--subtract", "sleep 0.05", "sleep 0.1"],
    ),
    (
        "reference command",
        &[
            "--runs=3",
            "--reference",
            "sleep 0.1",
            "sleep 0.2",
            "sleep 0.05",
        ],
    ),
    ("timeout", &["--runs=2", "--timeout=2s", "-i", "sleep 3"]),
    (
        "ignored failures",
        &["--runs=3", "-i", "--omit-failed-runs", "sleep 0.1"],
    ),
];

#[test]
fn exports_of_many_flag_combinations_match_the_schema() {
    let mut failures = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (case, args) in CASES {
        match perfratio_export(args) {
            Some(document) => {
                assert_valid(case, &document);
                for result in document["results"].as_array().unwrap() {
                    seen.extend(result.as_object().unwrap().keys().cloned());
                }
                seen.extend(document.as_object().unwrap().keys().cloned());
            }
            None => failures.push(*case),
        }
    }
    assert!(
        failures.is_empty(),
        "cases that failed to run: {failures:?}"
    );
    for (case, args) in real_cases() {
        let document = real_export(&args);
        assert_valid(case, &document);
        for result in document["results"].as_array().unwrap() {
            seen.extend(result.as_object().unwrap().keys().cloned());
        }
    }

    // every property of the schema is produced by at least one case, except the ones that
    // need a real host (see `NEEDS_REAL_HOST`), so no part of the schema goes untested
    let schema = schema();
    let mut declared: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
    declared.extend(
        schema["$defs"]["result"]["properties"]
            .as_object()
            .unwrap()
            .keys(),
    );
    let untested: Vec<_> = declared
        .into_iter()
        .filter(|k| !seen.contains(*k) && !NEEDS_REAL_HOST.contains(&k.as_str()))
        .collect();
    assert!(untested.is_empty(), "no case produces: {untested:?}");
}

/// Properties that no portable, deterministic case produces: `interrupted` and
/// `runs_planned` come from Ctrl-C (see `an_interrupted_run_matches_the_schema`), the energy
/// idle fields need readable RAPL, `counters_multiplexed_runs` and `counters_coverage` and `simd_scaling` a busy PMU,
/// `discarded_outliers` a noisy run, `omitted_failed_runs` a flaky command and `shell`
/// benchmarks with differing shells.
const NEEDS_REAL_HOST: &[&str] = &[
    "interrupted",
    "runs_planned",
    "idle_watts",
    "mean_net_energy_joules",
    "counters_multiplexed_runs",
    "counters_coverage",
    "simd_scaling",
    "discarded_outliers",
    "omitted_failed_runs",
    "shell",
    // `leftover_processes` needs a command that leaves a child behind (Unix case in
    // `real_cases`); `energy_per_instance_joules` is no longer written since 1.19
    "leftover_processes",
    "energy_per_instance_joules",
];

/// Cases that need real commands, which `--debug-mode` cannot simulate (`--output-metric`,
/// failing commands, OS resource counters). The benchmarked command is perfratio itself,
/// which exists on every platform and needs no POSIX shell.
fn real_cases() -> Vec<(&'static str, Vec<String>)> {
    let binary = assert_cmd::cargo_bin!("perfratio").display().to_string();
    let version = format!("\"{binary}\" --version");
    let failing = format!("\"{binary}\" --no-such-flag");
    let args = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
    vec![
        (
            "output metric",
            [
                args(&[
                    "--runs=3",
                    "-N",
                    "--output-metric",
                    "major=perfratio ([0-9]+)",
                ]),
                vec![version.clone()],
            ]
            .concat(),
        ),
        (
            "output metric as ranking metric",
            [
                args(&[
                    "--runs=3",
                    "-N",
                    "--output-metric",
                    "major=perfratio ([0-9]+)",
                    "--metric=custom:major",
                ]),
                vec![version.clone()],
            ]
            .concat(),
        ),
        (
            "resource usage",
            [args(&["--runs=3", "-N", "--resource-usage"]), vec![version]].concat(),
        ),
        (
            "ignored failing command",
            [args(&["--runs=3", "-N", "-i"]), vec![failing]].concat(),
        ),
        #[cfg(unix)]
        (
            "leftover processes",
            args(&["--runs=2", "sleep 36 & sleep 0.05"]),
        ),
    ]
}

fn real_export(args: &[String]) -> Value {
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

/// Runs one case; `None` if perfratio itself rejected the flags.
fn perfratio_export(args: &[&str]) -> Option<Value> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.json");
    let status = perfratio()
        .arg("--debug-mode")
        .args(args)
        .arg("--export-json")
        .arg(&path)
        .output()
        .unwrap();
    if !status.status.success() {
        eprintln!(
            "perfratio {args:?} failed:\n{}",
            String::from_utf8_lossy(&status.stderr)
        );
        return None;
    }
    Some(serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap())
}

#[test]
fn schema_version_matches_the_exporter() {
    let document = export(&["--runs=2", "sleep 0.1"]);
    let schema = schema();
    assert_eq!(
        schema["properties"]["schemaVersion"]["const"], document["schemaVersion"],
        "update docs/schema/perfratio-export.schema.json together with SCHEMA_VERSION"
    );
}

#[test]
fn energy_samples_counts_runs_with_a_sample() {
    let document = export(&["--runs=3", "--energy", "sleep 0.1"]);
    assert_eq!(document["results"][0]["energy_samples"], 3);
    assert_valid("energy samples", &document);
    let without = export(&["--runs=3", "sleep 0.1"]);
    assert!(without["results"][0].get("energy_samples").is_none());
}

#[test]
fn validation_object_matches_the_schema() {
    let document = export(&[
        "--runs=2",
        "--on-invalid=keep",
        "--expect-output-sha256",
        &"0".repeat(64),
        "sleep 0.1 0 0 0 hello",
    ]);
    assert_eq!(document["results"][0]["validation"]["runs_invalid"], 2);
    assert_valid("validation", &document);
}

#[test]
fn the_schema_rejects_a_document_that_breaks_the_contract() {
    let mut document = export(&["--runs=2", "sleep 0.1"]);
    let strict = validator(true);
    assert!(strict.is_valid(&document));

    // an undocumented key
    document["results"][0]["surprise"] = json!(1);
    assert!(!strict.is_valid(&document));
    document["results"][0]
        .as_object_mut()
        .unwrap()
        .remove("surprise");

    // a wrong type
    document["results"][0]["mean"] = json!("fast");
    assert!(!strict.is_valid(&document));
    document["results"][0]["mean"] = json!(0.1);

    // a missing guaranteed key
    document.as_object_mut().unwrap().remove("schemaVersion");
    assert!(!strict.is_valid(&document));
}

#[test]
fn the_published_schema_ignores_unknown_keys() {
    let mut document = export(&["--runs=2", "sleep 0.1"]);
    document["results"][0]["added_in_a_later_minor"] = json!([1, 2]);
    document["perfratio"]["measurements"]["gpu"] = json!("measured");
    assert!(validator(false).is_valid(&document));
}

#[cfg(unix)]
#[test]
fn an_interrupted_run_matches_the_schema() {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.json");
    let child = Command::new(assert_cmd::cargo_bin!("perfratio"))
        .args(["--runs=50", "--shell=none", "--export-json"])
        .arg(&path)
        .arg("sleep 0.1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn perfratio");
    std::thread::sleep(Duration::from_millis(800));
    // SAFETY: plain kill(2) on the child we spawned
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    child.wait_with_output().unwrap();
    let document: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(document["interrupted"], true);
    assert_valid("interrupted", &document);
}
