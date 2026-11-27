//! MCP server for agents: `run_benchmark`, `compare` and `capabilities` over stdio (#386).
//! Executes programs on request of the connected client; see `docs/specs/mcp-server.md`.

use std::io::{stdin, stdout};
use std::process::ExitCode;

use perfratio::mcp::{Config, Server};

const HELP: &str = "perfratio-mcp: MCP server (JSON-RPC 2.0 over stdio, no network)

USAGE: perfratio-mcp [--root DIR] [--allow PROGRAM]...

  --root DIR        files are read and written below DIR only (default: current directory)
  --allow PROGRAM   run only this argv[0] (repeatable). Without it any program may be run.

The client can make this process execute programs with your privileges. Start it
only from a host you trust and use --allow to narrow it. See docs/specs/mcp-server.md.";

fn main() -> ExitCode {
    let mut config = Config::default();
    let mut root = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "--root" => root = args.next(),
            "--allow" => match args.next() {
                Some(program) => config.allow.push(program),
                None => return usage("--allow needs a value"),
            },
            other => return usage(&format!("unknown argument '{other}'")),
        }
    }
    config.root = match root
        .map(Ok)
        .unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().into_owned()))
    {
        Ok(root) => root.into(),
        Err(e) => return usage(&format!("cannot determine the root directory: {e}")),
    };
    match Server::new(config).serve(stdin().lock(), stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("perfratio-mcp: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn usage(message: &str) -> ExitCode {
    eprintln!("perfratio-mcp: {message}\n\n{HELP}");
    ExitCode::from(2)
}
