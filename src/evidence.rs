//! Evidence bundle (`--evidence DIR`, `--verify-evidence DIR`): one directory with the
//! results of a run and everything needed to audit them, protected by SHA-256 hashes.
//! See `docs/specs/evidence-bundle.md` (#375).

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::util::sanitize::escape_control_chars;
use crate::validation::{sha256_hex, Sha256};

/// Largest `manifest.json` that `--verify-evidence` reads
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

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
    create_private_dir(dir).with_context(|| {
        format!(
            "Could not create the evidence directory '{}'",
            dir.display()
        )
    })
}

/// Creates `dir` (and missing parents with the default mode). A new `dir` is private (0700 on
/// Unix) because the bundle holds the command line and the environment. An existing empty
/// directory keeps its mode.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Write the bundle for the JSON export `results_json` into `dir`
pub fn write(dir: &Path, results_json: &[u8]) -> Result<()> {
    let files = bundle_files(results_json)?;
    let document: Value = serde_json::from_slice(results_json)?;
    let mut entries = Vec::new();
    for (name, content) in &files {
        write_new(&dir.join(name), content)
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
    write_new(&dir.join(MANIFEST), &bytes)
        .with_context(|| format!("Could not write '{}'", dir.join(MANIFEST).display()))
}

/// Create `path` (it must not exist, and a symlink in its place is never followed) and write
/// `content` to it
fn write_new(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    nofollow(&mut options);
    options.open(path)?.write_all(content)
}

#[cfg(unix)]
fn nofollow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn nofollow(_options: &mut OpenOptions) {}

/// Open `path` for reading without following a symlink (on Unix) and return the handle with
/// the metadata of the opened file, which must be a regular file
fn open_regular(path: &Path) -> std::io::Result<(File, u64)> {
    #[cfg(not(unix))]
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other("is a symbolic link"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    nofollow(&mut options);
    // O_NONBLOCK: opening a FIFO must not wait for a writer. The type is checked on the
    // handle below, and regular files never block, so the flag can stay set
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok((file, meta.len()))
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
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
        && name != MANIFEST
}

fn read_manifest(path: &Path) -> std::io::Result<Vec<u8>> {
    let (file, len) = open_regular(path)?;
    if len > MAX_MANIFEST_BYTES {
        return Err(std::io::Error::other(format!(
            "larger than {MAX_MANIFEST_BYTES} bytes"
        )));
    }
    let mut raw = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut raw)?;
    if raw.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(std::io::Error::other(format!(
            "larger than {MAX_MANIFEST_BYTES} bytes"
        )));
    }
    Ok(raw)
}

/// Size and SHA-256 of an open file, reading at most `limit + 1` bytes
fn hash_stream(file: File, limit: u64) -> std::io::Result<(u64, String)> {
    let mut reader = file.take(limit.saturating_add(1));
    let mut hasher = Sha256::default();
    let mut buf = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, hasher.finish_hex()))
}

/// Check every file of the bundle in `dir` against its manifest
///
/// The bundle is untrusted: every text in the report that comes from it is escaped.
pub fn verify(dir: &Path) -> Verification {
    let mut out = verify_raw(dir);
    for text in out.verified.iter_mut().chain(out.problems.iter_mut()) {
        if let std::borrow::Cow::Owned(escaped) = escape_control_chars(text) {
            *text = escaped;
        }
    }
    out.problems.sort();
    out
}

fn verify_raw(dir: &Path) -> Verification {
    let mut out = Verification::default();
    let manifest_path = dir.join(MANIFEST);
    let raw = match read_manifest(&manifest_path) {
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
        let (file, len) = match open_regular(&path) {
            Ok(opened) => opened,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.problems.push(format!("'{}' is missing", entry.path));
                continue;
            }
            Err(e) => {
                out.problems
                    .push(format!("cannot read '{}': {e}", entry.path));
                continue;
            }
        };
        if len != entry.bytes {
            out.problems.push(format!(
                "'{}' has {len} bytes, the manifest says {}",
                entry.path, entry.bytes
            ));
            continue;
        }
        match hash_stream(file, entry.bytes) {
            Ok((size, hash)) => {
                if size != entry.bytes {
                    out.problems.push(format!(
                        "'{}' has {size} bytes, the manifest says {}",
                        entry.path, entry.bytes
                    ));
                } else if hash != entry.sha256 {
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn new_evidence_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a").join("bundle");
        prepare(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

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
        for bad in [
            "",
            "..",
            "a/b",
            "..\\x",
            "C:x",
            "a\0b",
            "a\x1b[2Jb",
            "a\nb",
            "a\u{9b}b",
            MANIFEST,
        ] {
            assert!(!is_plain_name(bad), "{bad}");
        }
    }
}
