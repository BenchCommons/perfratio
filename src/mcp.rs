//! MCP server over stdio (#386): `run_benchmark`, `compare` and `capabilities` for agents.
//!
//! Protocol: JSON-RPC 2.0, one message per line, MCP revision 2026-07-28 (stateless, per-request
//! `_meta`, `server/discover`) and the legacy `initialize` handshake of 2025-11-25. Nothing here
//! opens a network socket. See `docs/specs/mcp-server.md` for the contract and the trust model.

use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::Benchmark;
use crate::benchmark::benchmark_result::BenchmarkResult;

/// Current protocol revision (stateless).
pub const MODERN_VERSION: &str = "2026-07-28";
/// Legacy revision served after an `initialize` request.
pub const LEGACY_VERSION: &str = "2025-11-25";
/// Largest `runs` and `warmup` a call may ask for.
pub const MAX_RUNS: u64 = 1000;
/// Largest argument vector accepted.
pub const MAX_ARGV: usize = 256;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// What the server may do.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// If not empty, `argv[0]` of any command to run must be one of these strings exactly.
    pub allow: Vec<String>,
    /// Files are read and written below this directory only.
    pub root: PathBuf,
}

/// Connection state: only the legacy handshake has any.
#[derive(Debug, Default)]
pub struct Server {
    config: Config,
    legacy_initialized: bool,
}

impl Server {
    /// Create a server; `config.root` is canonicalized when a file is used.
    pub fn new(config: Config) -> Self {
        Server {
            config,
            legacy_initialized: false,
        }
    }

    /// Read newline-delimited messages from `input` until EOF, write responses to `output`.
    ///
    /// # Errors
    /// Fails on an I/O error of `input` or `output`.
    pub fn serve<R: BufRead, W: Write>(&mut self, input: R, mut output: W) -> Result<()> {
        for line in input.lines() {
            let line = line.context("could not read from stdin")?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(&line) {
                let text = serde_json::to_string(&response)?;
                writeln!(output, "{text}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Handle one message; `None` for a notification.
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return Some(error(
                    Value::Null,
                    -32700,
                    &format!("parse error: {e}"),
                    None,
                ))
            }
        };
        let Some(object) = message.as_object() else {
            return Some(error(
                Value::Null,
                -32600,
                "batches and non-object messages are not supported",
                None,
            ));
        };
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // A response or something else: servers receive no responses in MCP
            return object
                .get("id")
                .map(|id| error(id.clone(), -32600, "invalid request: no method", None));
        };
        let id = match object.get("id") {
            None => {
                // Notification (initialized, cancelled, ...): nothing to answer. Calls are
                // handled one at a time, so a cancellation cannot interrupt a running benchmark.
                if method == "notifications/initialized" {
                    self.legacy_initialized = true;
                }
                return None;
            }
            Some(id) if id.is_string() || id.is_number() => id.clone(),
            Some(_) => {
                return Some(error(
                    Value::Null,
                    -32600,
                    "invalid request: id must be a string or a number",
                    None,
                ))
            }
        };
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        Some(match self.dispatch(method, &params) {
            Ok(mut result) => {
                if modern_version(&params).is_some() {
                    result["resultType"] = json!("complete");
                }
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            Err(e) => error(id, e.code, &e.message, e.data),
        })
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        if method == "initialize" {
            self.legacy_initialized = true;
            return Ok(json!({
                "protocolVersion": LEGACY_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": server_info(),
                "instructions": INSTRUCTIONS,
            }));
        }
        match modern_version(params) {
            Some(version) if version != MODERN_VERSION => {
                return Err(RpcError::new(-32022, "Unsupported protocol version").with_data(
                    json!({"supported": [MODERN_VERSION, LEGACY_VERSION], "requested": version}),
                ));
            }
            Some(_) => {}
            None if self.legacy_initialized => {}
            None => {
                return Err(RpcError::new(
                    -32602,
                    &format!("missing params._meta[\"{META_VERSION}\"] (or send initialize first)"),
                ));
            }
        }
        match method {
            "server/discover" => Ok(json!({
                "supportedVersions": [MODERN_VERSION, LEGACY_VERSION],
                "capabilities": {"tools": {}},
                "_meta": {META_SERVER_INFO: server_info()},
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => self.call_tool(params),
            _ => Err(RpcError::new(
                -32601,
                &format!("method not found: {method}"),
            )),
        }
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(-32602, "params.name is required"))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let outcome = match name {
            "capabilities" => Ok(self.capabilities()),
            "run_benchmark" => self.run_benchmark(arguments),
            "compare" => self.compare(arguments),
            _ => return Err(RpcError::new(-32602, &format!("unknown tool: {name}"))),
        };
        // Failures of the tool itself are results with isError, so the model can read them
        Ok(match outcome {
            Ok(value) => json!({
                "content": [{"type": "text", "text": value.to_string()}],
                "structuredContent": value,
            }),
            Err(e) => json!({
                "content": [{"type": "text", "text": format!("{e:#}")}],
                "isError": true,
            }),
        })
    }

    fn capabilities(&self) -> Value {
        let energy = crate::api::EnergyMeter::new().is_available();
        json!({
            "tool": {"name": "perfratio", "version": env!("CARGO_PKG_VERSION")},
            "verdictVersion": crate::verdict::VERDICT_VERSION,
            "protocolVersions": [MODERN_VERSION, LEGACY_VERSION],
            "metrics": {"wall": true, "cpu": true, "memory": true, "energy": energy},
            "limits": {"max_runs": MAX_RUNS, "max_warmup": MAX_RUNS, "max_argv": MAX_ARGV},
            "allowed_programs": self.config.allow,
            "root": self.config.root,
            "platform": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        })
    }

    fn run_benchmark(&self, arguments: Value) -> Result<Value> {
        let args: RunArgs = parse_arguments(arguments)?;
        let export = args.export_file.as_deref().map(|p| self.write_path(p));
        let export = export.transpose()?;
        let result = self.measure(&args.run)?;
        if let Some(path) = export {
            let json = serde_json::to_vec_pretty(&json!({"results": [&result]}))?;
            crate::export::write_to_file(&path_str(&path)?, &json)?;
        }
        Ok(json!({"result": result, "exported_to": args.export_file}))
    }

    fn compare(&self, arguments: Value) -> Result<Value> {
        let args: CompareArgs = parse_arguments(arguments)?;
        let baseline_path = self.read_path(&args.baseline_file)?;
        let baseline = crate::import::import_json(&path_str(&baseline_path)?)?;
        let current = match (&args.current_file, &args.run.argv) {
            (Some(_), Some(_)) => bail!("give either current_file or argv, not both"),
            (Some(file), None) => crate::import::import_json(&path_str(&self.read_path(file)?)?)?,
            (None, Some(_)) => vec![self.measure(&args.run)?],
            (None, None) => bail!("give current_file or argv"),
        };
        let comparison = crate::compare::compare(&baseline, &current);
        let document = crate::verdict::build(
            &comparison,
            None,
            &args.baseline_file,
            &crate::stats::settings::current(),
        );
        Ok(serde_json::to_value(document)?)
    }

    fn measure(&self, run: &RunSpec) -> Result<BenchmarkResult> {
        let argv = run
            .argv
            .as_deref()
            .ok_or_else(|| anyhow!("argv is required"))?;
        if argv.is_empty() || argv.len() > MAX_ARGV || argv.iter().any(|a| a.contains('\0')) {
            bail!("argv needs 1 to {MAX_ARGV} strings without NUL");
        }
        if !self.config.allow.is_empty() && !self.config.allow.contains(&argv[0]) {
            bail!(
                "program '{}' is not allowed; allowed: {}",
                argv[0],
                self.config.allow.join(", ")
            );
        }
        let runs = run.runs.unwrap_or(10);
        let warmup = run.warmup.unwrap_or(0);
        if !(1..=MAX_RUNS).contains(&runs) || warmup > MAX_RUNS {
            bail!("runs must be 1..={MAX_RUNS} and warmup 0..={MAX_RUNS}");
        }
        let report = Benchmark::new(argv.iter().cloned())
            .runs(runs)
            .warmup(warmup)
            .energy(run.energy.unwrap_or(false))
            .run()?;
        Ok(report.result)
    }

    /// An existing file below the root.
    fn read_path(&self, relative: &str) -> Result<PathBuf> {
        let path = self.join_relative(relative)?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("cannot open '{relative}'"))?;
        if !canonical.starts_with(self.root()?) {
            bail!("'{relative}' is outside the root directory");
        }
        Ok(canonical)
    }

    /// A file to create below the root: its directory must exist and lie below the root.
    fn write_path(&self, relative: &str) -> Result<PathBuf> {
        let path = self.join_relative(relative)?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow!("'{relative}' has no file name"))?
            .to_owned();
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("'{relative}' has no directory"))?
            .canonicalize()
            .with_context(|| format!("directory of '{relative}' does not exist"))?;
        if !parent.starts_with(self.root()?) {
            bail!("'{relative}' is outside the root directory");
        }
        Ok(parent.join(name))
    }

    fn root(&self) -> Result<PathBuf> {
        self.config
            .root
            .canonicalize()
            .with_context(|| "cannot resolve the root directory")
    }

    fn join_relative(&self, relative: &str) -> Result<PathBuf> {
        let path = Path::new(relative);
        let plain = path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if relative.is_empty() || path.is_absolute() || !plain {
            bail!("'{relative}' must be a relative path without '..'");
        }
        Ok(self.config.root.join(path))
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RunSpec {
    argv: Option<Vec<String>>,
    runs: Option<u64>,
    warmup: Option<u64>,
    energy: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    #[serde(flatten)]
    run: RunSpec,
    export_file: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompareArgs {
    baseline_file: String,
    current_file: Option<String>,
    #[serde(flatten)]
    run: RunSpec,
}

fn parse_arguments<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T> {
    serde_json::from_value(arguments).map_err(|e| anyhow!("invalid arguments: {e}"))
}

fn path_str(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("path is not valid UTF-8"))
}

fn modern_version(params: &Value) -> Option<&str> {
    params.get("_meta")?.get(META_VERSION)?.as_str()
}

fn server_info() -> Value {
    json!({"name": "perfratio-mcp", "version": env!("CARGO_PKG_VERSION")})
}

const INSTRUCTIONS: &str =
    "Benchmark a command with run_benchmark (export_file saves a baseline), \
then compare a changed command or export against it: the verdict says improved, regressed, \
unchanged, inconclusive or trade_off per metric with a confidence interval. inconclusive means \
measure more, not that nothing changed. run_benchmark and compare with argv execute programs.";

fn tool_definitions() -> Value {
    let run_properties = json!({
        "argv": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": MAX_ARGV,
                 "description": "Program and arguments. Run without a shell."},
        "runs": {"type": "integer", "minimum": 1, "maximum": MAX_RUNS, "default": 10},
        "warmup": {"type": "integer", "minimum": 0, "maximum": MAX_RUNS, "default": 0},
        "energy": {"type": "boolean", "default": false,
                   "description": "Measure energy where a sampler exists (see capabilities)."}
    });
    let mut compare_properties = run_properties.clone();
    compare_properties["baseline_file"] = json!({"type": "string",
        "description": "Export JSON of the baseline, relative to the server root."});
    compare_properties["current_file"] = json!({"type": "string",
        "description": "Export JSON of the current run. Alternative to argv: nothing is executed."});
    let mut run_tool_properties = run_properties;
    run_tool_properties["export_file"] = json!({"type": "string",
        "description": "Write the result as an export JSON (usable as baseline_file)."});
    json!([
        {
            "name": "capabilities",
            "description": "Version, supported protocol and verdict versions, metrics available \
    on this host, limits and the program allow-list. Executes nothing.",
            "inputSchema": {"type": "object", "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        },
        {
            "name": "run_benchmark",
            "description": "Run a command repeatedly and return its timing, CPU, memory and \
    statistics. Executes the program with the privileges of the server.",
            "inputSchema": {"type": "object", "properties": run_tool_properties,
                            "required": ["argv"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": true,
                            "openWorldHint": true}
        },
        {
            "name": "compare",
            "description": "Compare a command (argv, executed) or an export (current_file) with \
    a baseline export and return the verdict document (docs/schema/verdict.schema.json).",
            "inputSchema": {"type": "object", "properties": compare_properties,
                            "required": ["baseline_file"], "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": true,
                            "openWorldHint": true}
        }
    ])
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: &str) -> Self {
        RpcError {
            code,
            message: message.to_string(),
            data: None,
        }
    }
    fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error})
}
