//! Evidence bundle (`--evidence DIR`, `--verify-evidence DIR`): one directory with the
//! results of a run and everything needed to audit them, protected by SHA-256 hashes.
//! See `docs/specs/evidence-bundle.md` (#375).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::validation::sha256_hex;

/// Name of the format in `manifest.json` (`format`)
pub const FORMAT: &str = "perfratio-evidence";
/// Version of the bundle format, independent of the JSON export `schemaVersion`.
/// A change of the major number is breaking.
pub const FORMAT_VERSION: &str = "1.0";
/// The manifest, written last; it lists every other file
pub const MANIFEST: &str = "manifest.json";
/// Files every bundle contains, in the order of the manifest
pub const REQUIRED_FILES: [&str; 7] = [
    "calibration.json",
    "command.json",
    "environment.json",
    "quality.json",
    "results.json",
    "samples.jsonl",
    "validation.json",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    /// File name inside the bundle (no directories)
    pub path: String,
    /// SHA-256 of the file content, 64 lowercase hex digits
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub format: String,
    pub format_version: String,
    pub perfratio_version: String,
    /// `schemaVersion` of `results.json`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results_schema_version: Option<String>,
    /// Start of the run (`perfratio.started_at` of `results.json`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub files: Vec<FileEntry>,
}

fn pretty(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// `{ "available": false }` or `{ "available": true, "data": ... }`
fn section(data: Option<Value>) -> Value {
    match data {
        Some(data) => json!({ "available": true, "data": data }),
        None => json!({ "available": false }),
    }
}

/// One line per result and run: `command_index`, `run` and every per-run series of the
/// result (`times` as `time`, `user_times`, `exit_codes`, ...)
fn samples_jsonl(results: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, result) in results.iter().enumerate() {
        let Some(times) = result.get("times").and_then(Value::as_array) else {
            continue;
        };
        let series: Vec<(&str, &Vec<Value>)> = result
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| {
                let array = value.as_array()?;
                (array.len() == times.len()
                    && array.iter().all(|v| !v.is_object() && !v.is_array()))
                .then_some((key.as_str(), array))
            })
            .collect();
        for run in 0..times.len() {
            let mut line = Map::new();
            line.insert("command_index".into(), json!(index));
            line.insert("run".into(), json!(run));
            for (key, array) in &series {
                let name = if *key == "times" { "time" } else { key };
                line.insert(name.to_string(), array[run].clone());
            }
            out.extend(serde_json::to_vec(&Value::Object(line)).unwrap_or_default());
            out.push(b'\n');
        }
    }
    out
}

/// All files of a bundle for the JSON export `results_json`, without the manifest
pub fn bundle_files(results_json: &[u8]) -> Result<Vec<(&'static str, Vec<u8>)>> {
    let document: Value =
        serde_json::from_slice(results_json).context("The JSON export is not valid JSON")?;
    let meta = document.get("perfratio");
    let results: &[Value] = document
        .get("results")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let per_result = |key: &str| -> Option<Value> {
        let entries: Vec<Value> = results
            .iter()
            .enumerate()
            .filter_map(|(index, result)| {
                let data = result.get(key)?;
                Some(json!({
                    "command_index": index,
                    "command": result.get("command"),
                    key: data,
                }))
            })
            .collect();
        (!entries.is_empty()).then_some(Value::Array(entries))
    };
    let field = |name: &str| meta.and_then(|m| m.get(name)).cloned();

    let environment = json!({
        "environment": field("environment"),
        "environment_fingerprint": field("environment_fingerprint"),
        "system": field("system"),
        "measurements": field("measurements"),
        "execution": field("execution"),
        "statistics": field("statistics"),
        "meta": field("meta"),
    });
    let command = json!({
        "argv": field("command_line"),
        "perfratio_version": field("version"),
        "started_at": field("started_at"),
        "labels": field("labels"),
    });
    Ok(vec![
        ("calibration.json", pretty(&section(field("calibration")))),
        ("command.json", pretty(&command)),
        ("environment.json", pretty(&environment)),
        ("quality.json", pretty(&section(per_result("quality")))),
        ("results.json", results_json.to_vec()),
        ("samples.jsonl", samples_jsonl(results)),
        (
            "validation.json",
            pretty(&section(per_result("validation"))),
        ),
    ])
}

/// Fail before the benchmarks run if `dir` cannot receive a bundle: it must be absent or an
/// empty directory. The directory is created.
pub fn prepare(dir: &Path) -> Result<()> {
    if dir.exists() {
        if !dir.is_dir() {
            bail!("'{}' exists and is not a directory", dir.display());
        }
        if fs::read_dir(dir)?.next().is_some() {
            bail!(
                "The evidence directory '{}' is not empty: perfratio never overwrites a bundle",
                dir.display()
            );
        }
    }
    fs::create_dir_all(dir).with_context(|| {
        format!(
            "Could not create the evidence directory '{}'",
            dir.display()
        )
    })
}

/// Write the bundle for the JSON export `results_json` into `dir`
pub fn write(dir: &Path, results_json: &[u8]) -> Result<()> {
    let files = bundle_files(results_json)?;
    let document: Value = serde_json::from_slice(results_json)?;
    let mut entries = Vec::new();
    for (name, content) in &files {
        fs::write(dir.join(name), content)
            .with_context(|| format!("Could not write '{}'", dir.join(name).display()))?;
        entries.push(FileEntry {
            path: (*name).to_string(),
            sha256: sha256_hex(content),
            bytes: content.len() as u64,
        });
    }
    let manifest = Manifest {
        format: FORMAT.to_string(),
        format_version: FORMAT_VERSION.to_string(),
        perfratio_version: env!("CARGO_PKG_VERSION").to_string(),
        results_schema_version: document
            .get("schemaVersion")
            .and_then(Value::as_str)
            .map(str::to_string),
        created_at: document
            .pointer("/perfratio/started_at")
            .and_then(Value::as_str)
            .map(str::to_string),
        files: entries,
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    fs::write(dir.join(MANIFEST), bytes)
        .with_context(|| format!("Could not write '{}'", dir.join(MANIFEST).display()))
}

/// Outcome of `--verify-evidence`
#[derive(Debug, Default)]
pub struct Verification {
    /// Files whose hash and size match the manifest
    pub verified: Vec<String>,
    /// Everything that is wrong; empty if the bundle is intact
    pub problems: Vec<String>,
    /// SHA-256 of `manifest.json`, to pin a bundle
    pub manifest_sha256: Option<String>,
}

impl Verification {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// A manifest path is a plain file name: no directories, no `..`
fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && name != MANIFEST
}

/// Check every file of the bundle in `dir` against its manifest
pub fn verify(dir: &Path) -> Verification {
    let mut out = Verification::default();
    let manifest_path = dir.join(MANIFEST);
    let raw = match fs::read(&manifest_path) {
        Ok(raw) => raw,
        Err(e) => {
            out.problems
                .push(format!("cannot read '{}': {e}", manifest_path.display()));
            return out;
        }
    };
    out.manifest_sha256 = Some(sha256_hex(&raw));
    let manifest: Manifest = match serde_json::from_slice(&raw) {
        Ok(manifest) => manifest,
        Err(e) => {
            out.problems.push(format!("{MANIFEST} is invalid: {e}"));
            return out;
        }
    };
    if manifest.format != FORMAT {
        out.problems.push(format!(
            "{MANIFEST}: format is '{}', expected '{FORMAT}'",
            manifest.format
        ));
        return out;
    }
    let major = |v: &str| v.split('.').next().map(str::to_string);
    if major(&manifest.format_version) != major(FORMAT_VERSION) {
        out.problems.push(format!(
            "{MANIFEST}: unsupported format_version '{}' (this perfratio reads {FORMAT_VERSION})",
            manifest.format_version
        ));
        return out;
    }

    let mut listed = BTreeSet::new();
    for entry in &manifest.files {
        if !is_plain_name(&entry.path) {
            out.problems
                .push(format!("{MANIFEST}: invalid file name '{}'", entry.path));
            continue;
        }
        if !listed.insert(entry.path.clone()) {
            out.problems
                .push(format!("{MANIFEST}: '{}' is listed twice", entry.path));
            continue;
        }
        let path = dir.join(&entry.path);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                out.problems
                    .push(format!("'{}' is not a regular file", entry.path));
                continue;
            }
            Err(_) => {
                out.problems.push(format!("'{}' is missing", entry.path));
                continue;
            }
        }
        match fs::read(&path) {
            Ok(content) => {
                if content.len() as u64 != entry.bytes {
                    out.problems.push(format!(
                        "'{}' has {} bytes, the manifest says {}",
                        entry.path,
                        content.len(),
                        entry.bytes
                    ));
                } else if sha256_hex(&content) != entry.sha256 {
                    out.problems
                        .push(format!("'{}' does not match its sha256", entry.path));
                } else {
                    out.verified.push(entry.path.clone());
                }
            }
            Err(e) => out
                .problems
                .push(format!("cannot read '{}': {e}", entry.path)),
        }
    }
    for required in REQUIRED_FILES {
        if !listed.contains(required) {
            out.problems.push(format!(
                "{MANIFEST} does not list the required '{required}'"
            ));
        }
    }
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name != MANIFEST && !listed.contains(&name) {
                    out.problems
                        .push(format!("'{name}' is not listed in {MANIFEST}"));
                }
            }
        }
        Err(e) => out.problems.push(format!("cannot read the directory: {e}")),
    }
    out.problems.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_have_one_line_per_run() {
        let results = vec![json!({
            "command": "a",
            "times": [0.1, 0.2],
            "user_times": [0.01, 0.02],
            "exit_codes": [0, 0],
            "mean": 0.15,
        })];
        let text = String::from_utf8(samples_jsonl(&results)).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["run"], 1);
        assert_eq!(lines[1]["time"], 0.2);
        assert_eq!(lines[1]["user_times"], 0.02);
        assert!(lines[0].get("mean").is_none());
    }

    #[test]
    fn plain_names_only() {
        assert!(is_plain_name("results.json"));
        for bad in ["", "..", "a/b", "..\\x", "C:x", MANIFEST] {
            assert!(!is_plain_name(bad), "{bad}");
        }
    }
}
