//! Collector plugin protocol (#384). See `docs/specs/collector-plugins.md`.
//!
//! A collector is an external program (`--collector-plugin PATH`) that measures something
//! perfratio cannot, such as a GPU power sensor. perfratio starts it once, speaks JSON lines to
//! it over stdin/stdout, and brackets every timed run with `start` / `stop` / `read`:
//!
//! ```text
//! perfratio -> {"type":"hello","protocol":1}
//! plugin    -> {"type":"hello","protocol":1,"name":"gpu","metrics":[{"id":"energy","unit":"J"}]}
//! perfratio -> {"type":"start","run":0}          plugin -> {"type":"ok"}
//!              (the command runs)
//! perfratio -> {"type":"stop","run":0}           plugin -> {"type":"ok"}
//! perfratio -> {"type":"read","run":0}           plugin -> {"type":"metrics","values":{"energy":1.5}}
//! perfratio -> {"type":"shutdown"}               (stdin is closed, the plugin exits)
//! ```
//!
//! The plugin is trusted code: it runs with the user's privileges and is started without a
//! shell. Everything it sends is untrusted data: names, units and values are validated here
//! and a violation aborts the benchmark.
//!
//! The metrics of every collector (plugin or built-in adapter, see `crate::valgrind`) are
//! exported in `results[].collector_metrics` and described by `collector.<name>.<id>`
//! descriptors in `perfratio.metrics[]`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::metric_registry::{Direction, Kind, Scope};

/// Version of the collector protocol spoken by this perfratio.
pub const PROTOCOL_VERSION: u32 = 1;
/// Longest accepted reply line, in bytes.
pub const MAX_LINE: usize = 64 * 1024;
/// Most metrics one plugin may declare.
pub const MAX_METRICS: usize = 64;
/// Time a plugin has to answer `hello`.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Time a plugin has to answer any other request.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// What the descriptor of a collector metric says.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    pub unit: String,
    pub direction: Direction,
    pub kind: Kind,
    pub scope: Scope,
    pub source: String,
}

fn meta_table() -> MutexGuard<'static, HashMap<String, Meta>> {
    static TABLE: std::sync::OnceLock<Mutex<HashMap<String, Meta>>> = std::sync::OnceLock::new();
    TABLE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Declares the descriptor data of the exported key `<collector>.<metric>`.
pub fn declare(key: &str, meta: Meta) {
    meta_table().insert(key.to_string(), meta);
}

/// Descriptor data declared for the exported key `<collector>.<metric>`.
pub fn meta(key: &str) -> Option<Meta> {
    meta_table().get(key).cloned()
}

/// `true` for names that are safe as one segment of an exported key: `[a-z][a-z0-9_]{0,31}`.
pub fn valid_segment(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Units are 1 to 16 characters without whitespace, control characters, `[`, `]`, `,` or `=`
/// (the rule of `--output-metric`).
pub fn valid_unit(unit: &str) -> bool {
    !unit.is_empty()
        && unit.chars().count() <= 16
        && unit
            .chars()
            .all(|c| !c.is_control() && !c.is_whitespace() && !"[]=,".contains(c))
}

/// One metric a plugin declared in its `hello` reply.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricDecl {
    pub id: String,
    pub meta: Meta,
}

#[derive(Debug, Deserialize)]
struct RawMetric {
    id: String,
    unit: String,
    #[serde(default)]
    direction: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawHello {
    name: String,
    metrics: Vec<RawMetric>,
}

fn parse_direction(value: Option<&str>) -> Result<Direction> {
    Ok(match value.unwrap_or("lower_is_better") {
        "lower_is_better" => Direction::LowerIsBetter,
        "higher_is_better" => Direction::HigherIsBetter,
        "neutral" => Direction::Neutral,
        other => bail!("unknown direction '{other}'"),
    })
}

fn parse_kind(value: Option<&str>) -> Result<Kind> {
    Ok(match value.unwrap_or("measured") {
        "measured" => Kind::Measured,
        "estimated" => Kind::Estimated,
        "simulated" => Kind::Simulated,
        "derived" => Kind::Derived,
        other => bail!("unknown kind '{other}'"),
    })
}

fn parse_scope(value: Option<&str>) -> Result<Scope> {
    Ok(match value.unwrap_or("system") {
        "process" => Scope::Process,
        "process_tree" => Scope::ProcessTree,
        "batch" => Scope::Batch,
        "package" => Scope::Package,
        "system" => Scope::System,
        other => bail!("unknown scope '{other}'"),
    })
}

/// Validates the `hello` reply of a plugin: protocol version, name and metric declarations.
pub fn parse_hello(line: &str) -> Result<(String, Vec<MetricDecl>)> {
    let value: Value = serde_json::from_str(line).context("malformed 'hello' reply")?;
    check_reply_type(&value, "hello")?;
    // Checked first: a plugin of another protocol version may be shaped differently
    let protocol = value.get("protocol").and_then(Value::as_u64);
    ensure!(
        protocol == Some(u64::from(PROTOCOL_VERSION)),
        "the plugin speaks protocol {}, this perfratio speaks {PROTOCOL_VERSION}",
        protocol.map_or("none".to_string(), |p| p.to_string())
    );
    let hello: RawHello = serde_json::from_value(value).context("malformed 'hello' reply")?;
    ensure!(
        valid_segment(&hello.name),
        "invalid plugin name '{}': use 1 to 32 characters of [a-z0-9_], starting with a letter",
        hello.name.escape_debug()
    );
    ensure!(
        !crate::valgrind::TOOL_NAMES.contains(&hello.name.as_str()),
        "the plugin name '{}' is reserved for a built-in collector",
        hello.name
    );
    ensure!(
        !hello.metrics.is_empty() && hello.metrics.len() <= MAX_METRICS,
        "a plugin must declare between 1 and {MAX_METRICS} metrics"
    );
    let source = format!("plugin:{}", hello.name);
    let mut seen = BTreeSet::new();
    let mut metrics = Vec::new();
    for raw in hello.metrics {
        let context = || format!("metric '{}'", raw.id.escape_debug());
        ensure!(valid_segment(&raw.id), "{}: invalid id", context());
        ensure!(seen.insert(raw.id.clone()), "{}: declared twice", context());
        ensure!(
            valid_unit(&raw.unit),
            "{}: invalid unit '{}'",
            context(),
            raw.unit.escape_debug()
        );
        let meta = Meta {
            unit: raw.unit.clone(),
            direction: parse_direction(raw.direction.as_deref()).with_context(context)?,
            kind: parse_kind(raw.kind.as_deref()).with_context(context)?,
            scope: parse_scope(raw.scope.as_deref()).with_context(context)?,
            source: source.clone(),
        };
        metrics.push(MetricDecl { id: raw.id, meta });
    }
    Ok((hello.name, metrics))
}

/// Validates a `metrics` reply: only declared ids, finite numbers or `null`.
pub fn parse_metrics(line: &str, declared: &[MetricDecl]) -> Result<BTreeMap<String, f64>> {
    let value: Value = serde_json::from_str(line).context("malformed 'metrics' reply")?;
    check_reply_type(&value, "metrics")?;
    let values = value
        .get("values")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("the 'metrics' reply has no 'values' object"))?;
    let mut out = BTreeMap::new();
    for (id, value) in values {
        ensure!(
            declared.iter().any(|m| &m.id == id),
            "the plugin reported the undeclared metric '{}'",
            id.escape_debug()
        );
        match value {
            Value::Null => {}
            Value::Number(n) => {
                let v = n.as_f64().filter(|v| v.is_finite());
                out.insert(
                    id.clone(),
                    v.ok_or_else(|| anyhow!("metric '{id}': the value is not a finite number"))?,
                );
            }
            _ => bail!("metric '{id}': the value must be a number or null"),
        }
    }
    Ok(out)
}

/// An `error` reply fails with the plugin's message; any other type than `expected` is a
/// protocol violation.
fn check_reply_type(value: &Value, expected: &str) -> Result<()> {
    match value.get("type").and_then(Value::as_str) {
        Some(t) if t == expected => Ok(()),
        Some("error") => {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("no message");
            // The message is untrusted text: keep it short and printable
            let message: String = message
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            bail!("the plugin reported an error: {message}")
        }
        other => bail!("expected a '{expected}' reply, got {other:?}"),
    }
}

type Line = std::io::Result<Option<String>>;

/// Reads one line of at most [`MAX_LINE`] bytes; `None` at end of stream.
fn read_line_limited<R: BufRead>(reader: &mut R) -> Line {
    let mut buffer = Vec::new();
    let read = reader
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut buffer)?;
    if read == 0 {
        return Ok(None);
    }
    if buffer.len() > MAX_LINE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("reply longer than {MAX_LINE} bytes"),
        ));
    }
    String::from_utf8(buffer)
        .map(|line| Some(line.trim_end().to_string()))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "reply is not UTF-8"))
}

/// A running plugin.
pub struct Session {
    path: PathBuf,
    name: String,
    metrics: Vec<MetricDecl>,
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Line>,
}

impl Session {
    /// Starts the plugin at `path` (no shell, no arguments) and performs the handshake.
    pub fn start(path: &Path) -> Result<Session> {
        // A bare name would be searched on PATH: the contract is an explicit path
        if path.parent().is_none_or(|p| p.as_os_str().is_empty()) {
            bail!(
                "the collector plugin {path:?} is not a path: give an explicit path such as ./{}",
                path.display()
            );
        }
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("could not start the collector plugin {path:?}"))?;
        let stdin = child.stdin.take();
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || loop {
            let line = read_line_limited(&mut stdout);
            let done = !matches!(line, Ok(Some(_)));
            if sender.send(line).is_err() || done {
                break;
            }
        });
        let mut session = Session {
            path: path.to_path_buf(),
            name: String::new(),
            metrics: Vec::new(),
            child,
            stdin,
            replies,
        };
        session.send(&json!({"type": "hello", "protocol": PROTOCOL_VERSION}))?;
        let line = session.receive(HELLO_TIMEOUT)?;
        let (name, metrics) =
            parse_hello(&line).with_context(|| format!("collector plugin {path:?}"))?;
        session.name = name;
        session.metrics = metrics;
        Ok(session)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn metrics(&self) -> &[MetricDecl] {
        &self.metrics
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("the plugin is shut down"))?;
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        stdin
            .write_all(&line)
            .and_then(|()| stdin.flush())
            .with_context(|| format!("could not write to the collector plugin {:?}", self.path))
    }

    fn receive(&mut self, timeout: Duration) -> Result<String> {
        match self.replies.recv_timeout(timeout) {
            Ok(Ok(Some(line))) => Ok(line),
            Ok(Ok(None)) | Err(RecvTimeoutError::Disconnected) => bail!(
                "the collector plugin {:?} closed its output unexpectedly",
                self.path
            ),
            Ok(Err(e)) => {
                Err(e).with_context(|| format!("collector plugin {:?} sent bad data", self.path))
            }
            Err(RecvTimeoutError::Timeout) => {
                // A stuck plugin must not hang the benchmark
                let _ = self.child.kill();
                bail!(
                    "the collector plugin {:?} did not answer within {} s",
                    self.path,
                    timeout.as_secs()
                )
            }
        }
    }

    fn request_ok(&mut self, kind: &str, run: u64) -> Result<()> {
        self.send(&json!({"type": kind, "run": run}))?;
        let line = self.receive(REPLY_TIMEOUT)?;
        let value: Value = serde_json::from_str(&line)
            .with_context(|| format!("malformed '{kind}' reply from '{}'", self.name))?;
        check_reply_type(&value, "ok").with_context(|| format!("collector '{}'", self.name))
    }

    pub fn start_run(&mut self, run: u64) -> Result<()> {
        self.request_ok("start", run)
    }

    pub fn stop_run(&mut self, run: u64) -> Result<()> {
        self.request_ok("stop", run)
    }

    /// Reads the metrics of the run that was stopped.
    pub fn read_run(&mut self, run: u64) -> Result<BTreeMap<String, f64>> {
        self.send(&json!({"type": "read", "run": run}))?;
        let line = self.receive(REPLY_TIMEOUT)?;
        parse_metrics(&line, &self.metrics).with_context(|| format!("collector '{}'", self.name))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Ask politely, then close stdin; do not wait for a stuck plugin
        let _ = self.send(&json!({"type": "shutdown"}));
        self.stdin = None;
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

fn sessions() -> MutexGuard<'static, Vec<Session>> {
    static SESSIONS: Mutex<Vec<Session>> = Mutex::new(Vec::new());
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the plugins of `--collector-plugin` and declares their metrics. Called once, before
/// the first benchmark.
pub fn init(paths: &[PathBuf]) -> Result<()> {
    let mut started = Vec::new();
    for path in paths {
        let session = Session::start(path)?;
        ensure!(
            !started.iter().any(|s: &Session| s.name() == session.name()),
            "two collector plugins are named '{}'",
            session.name()
        );
        for metric in session.metrics() {
            declare(
                &format!("{}.{}", session.name(), metric.id),
                metric.meta.clone(),
            );
        }
        started.push(session);
    }
    ACTIVE.store(!started.is_empty(), Ordering::SeqCst);
    *sessions() = started;
    Ok(())
}

/// Stops the plugins (they receive `shutdown`).
pub fn shutdown() {
    ACTIVE.store(false, Ordering::SeqCst);
    sessions().clear();
}

/// `true` if plugins are running.
pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// Exported keys (`<name>.<id>`) of the running plugins' metrics.
pub fn plugin_keys() -> BTreeSet<String> {
    sessions()
        .iter()
        .flat_map(|s| {
            s.metrics()
                .iter()
                .map(|m| format!("{}.{}", s.name(), m.id))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Tells every plugin that run `run` starts. Call right before the command is spawned.
pub fn start_run(run: u64) -> Result<()> {
    let mut guard = sessions();
    for (i, session) in guard.iter_mut().enumerate() {
        if let Err(e) = session.start_run(run) {
            // Plugins that already started are stopped, so the protocol state stays clean
            for started in guard.iter_mut().take(i) {
                let _ = started.stop_run(run);
            }
            return Err(e);
        }
    }
    Ok(())
}

/// Stops every plugin and reads its metrics: the map of exported keys to values.
pub fn finish_run(run: u64) -> Result<BTreeMap<String, f64>> {
    let mut guard = sessions();
    let mut first_error = None;
    for session in guard.iter_mut() {
        if let Err(e) = session.stop_run(run) {
            first_error.get_or_insert(e);
        }
    }
    if let Some(e) = first_error {
        return Err(e);
    }
    let mut out = BTreeMap::new();
    for session in guard.iter_mut() {
        for (id, value) in session.read_run(run)? {
            out.insert(format!("{}.{id}", session.name()), value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = r#"{"type":"hello","protocol":1,"name":"gpu","metrics":[
        {"id":"energy","unit":"J"},
        {"id":"util","unit":"percent","direction":"neutral","kind":"estimated","scope":"package"}]}"#;

    #[test]
    fn plugin_without_a_directory_part_is_rejected() {
        let err = Session::start(Path::new("gpu-plugin")).err().unwrap();
        assert!(err.to_string().contains("explicit path"), "{err}");
    }

    #[test]
    fn hello_declares_metrics_with_defaults() {
        let (name, metrics) = parse_hello(HELLO).unwrap();
        assert_eq!(name, "gpu");
        assert_eq!(metrics[0].meta.direction, Direction::LowerIsBetter);
        assert_eq!(metrics[0].meta.kind, Kind::Measured);
        assert_eq!(metrics[0].meta.scope, Scope::System);
        assert_eq!(metrics[0].meta.source, "plugin:gpu");
        assert_eq!(metrics[1].meta.direction, Direction::Neutral);
        assert_eq!(metrics[1].meta.kind, Kind::Estimated);
    }

    #[test]
    fn hello_is_validated() {
        for (bad, why) in [
            (
                r#"{"type":"hello","protocol":2,"name":"g","metrics":[{"id":"a","unit":"J"}]}"#,
                "protocol",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"G!","metrics":[{"id":"a","unit":"J"}]}"#,
                "name",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"cachegrind","metrics":[{"id":"a","unit":"J"}]}"#,
                "reserved",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"g","metrics":[]}"#,
                "between",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"g","metrics":[{"id":"a.b","unit":"J"}]}"#,
                "invalid id",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"g","metrics":[{"id":"a","unit":"J"},{"id":"a","unit":"J"}]}"#,
                "twice",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"g","metrics":[{"id":"a","unit":"a b"}]}"#,
                "unit",
            ),
            (
                r#"{"type":"hello","protocol":1,"name":"g","metrics":[{"id":"a","unit":"J","kind":"magic"}]}"#,
                "kind",
            ),
            (r#"{"type":"ok"}"#, "hello"),
            ("not json", "malformed"),
        ] {
            let error = format!("{:#}", parse_hello(bad).unwrap_err());
            assert!(error.contains(why), "{bad}: {error}");
        }
    }

    #[test]
    fn metrics_reply_is_validated() {
        let (_, declared) = parse_hello(HELLO).unwrap();
        let ok = parse_metrics(
            r#"{"type":"metrics","values":{"energy":1.5,"util":null}}"#,
            &declared,
        )
        .unwrap();
        assert_eq!(ok.get("energy"), Some(&1.5));
        assert!(!ok.contains_key("util"), "null means no value");
        for bad in [
            r#"{"type":"metrics","values":{"other":1}}"#,
            r#"{"type":"metrics","values":{"energy":"1"}}"#,
            r#"{"type":"metrics","values":{"energy":1e999}}"#,
            r#"{"type":"metrics"}"#,
            r#"{"type":"error","message":"sensor lost\u0007"}"#,
        ] {
            assert!(parse_metrics(bad, &declared).is_err(), "{bad}");
        }
        let error = parse_metrics(
            r#"{"type":"error","message":"sensor lost\u0007"}"#,
            &declared,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("sensor lost") && !error.contains('\u{7}'));
    }

    #[test]
    fn lines_are_bounded() {
        let long = format!("{}\n", "x".repeat(MAX_LINE + 10));
        assert!(read_line_limited(&mut long.as_bytes()).is_err());
        let mut short = "ok\nnext\n".as_bytes();
        assert_eq!(
            read_line_limited(&mut short).unwrap().as_deref(),
            Some("ok")
        );
        assert_eq!(read_line_limited(&mut "".as_bytes()).unwrap(), None);
    }
}
