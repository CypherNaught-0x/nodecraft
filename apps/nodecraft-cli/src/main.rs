//! `nodecraft-cli`: the headless NodeCraft frontend.
//!
//! `mcp` serves the engine's commands to MCP clients over stdio. Running graphs and techniques,
//! `quote`, `inspect`, `commands`, `providers`, `comfy` and `serve` arrive with their milestones
//! (docs/plan.md §4.8).

use std::hash::{BuildHasher, RandomState};
use std::process::ExitCode;
use std::sync::Arc;

use nodecraft_automation::mcp::{McpServer, ServeEnd, serve};
use nodecraft_engine::commands::Services;
use nodecraft_engine::secrets::{KeyringStore, register_key_service};
use nodecraft_types::{CanvasId, ProjectId};

const USAGE: &str = "\
usage: nodecraft-cli <command>

commands:
  mcp             serve NodeCraft to an MCP client over stdin/stdout (a new, empty project)
  mcp --bridge [token-file]
                  serve the running app instead (started with `nodecraft --control`); the
                  token file defaults to control/control.json in the app's settings folder
  --version, -V   print the version, commit and build date
  --help, -h      print this help
";

/// 16 random bytes from the standard library's per-process random hasher keys and a counter:
/// unique ids without another dependency. Not for secrets.
fn random_bytes() -> impl FnMut() -> [u8; 16] {
    let (a, b) = (RandomState::new(), RandomState::new());
    let mut counter: u64 = 0;
    move || {
        counter = counter.wrapping_add(1);
        let mut out = [0u8; 16];
        let (hi, lo) = out.split_at_mut(8);
        hi.copy_from_slice(&a.hash_one(counter).to_le_bytes());
        lo.copy_from_slice(&b.hash_one(counter).to_le_bytes());
        out
    }
}

/// Serves `server` on stdio until the client closes it.
fn serve_stdio(server: &mut McpServer) -> ExitCode {
    // Stdout carries the protocol only; diagnostics go to stderr.
    match serve(server, &mut std::io::stdin().lock(), &mut std::io::stdout().lock()) {
        ServeEnd::Closed => ExitCode::SUCCESS,
        ServeEnd::Io(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The token file the app writes: `control/control.json` in its settings folder.
fn default_token_file() -> Option<std::path::PathBuf> {
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Auto);
    store.dir().map(|dir| dir.join("control").join("control.json"))
}

fn bridge(token_file: Option<&str>) -> ExitCode {
    let Some(path) = token_file.map(std::path::PathBuf::from).or_else(default_token_file) else {
        eprintln!("error: no settings folder to find the app's control token in; pass the token file");
        return ExitCode::FAILURE;
    };
    let client = match nodecraft_automation::control::Client::from_token_file(&path) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("error: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    serve_stdio(&mut McpServer::bridge(client, Box::new(random_bytes())))
}

fn mcp() -> ExitCode {
    let mut new_id = random_bytes();
    let session = nodecraft_automation::blank_session(ProjectId::from_bytes(new_id()), CanvasId::from_bytes(new_id()));
    let mut services = Services::default();
    let session = match session.and_then(|s| register_key_service(&mut services, Arc::new(KeyringStore)).map(|()| s)) {
        Ok(s) => s.with_services(services),
        Err(e) => {
            eprintln!("error: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    serve_stdio(&mut McpServer::new(session, Box::new(new_id)))
}

fn main() -> ExitCode {
    let args =
        match std::env::args_os().skip(1).enumerate().map(|(index, arg)| arg.into_string().map_err(|_| index.saturating_add(1))).collect::<Result<Vec<_>, _>>()
        {
            Ok(args) => args,
            Err(index) => {
                eprintln!("error: argument {index} must be valid UTF-8");
                return ExitCode::FAILURE;
            }
        };
    match args.first().map(String::as_str) {
        Some("--version" | "-V" | "version") => {
            println!("nodecraft-cli {}", nodecraft_engine::build_info::long_version());
            ExitCode::SUCCESS
        }
        Some("mcp") => match args.get(1..).unwrap_or_default() {
            [] => mcp(),
            [flag] if flag == "--bridge" => bridge(None),
            [flag, path] if flag == "--bridge" => bridge(Some(path)),
            _ => {
                eprintln!("error: `mcp` takes only `--bridge [token-file]`\n\n{USAGE}");
                ExitCode::FAILURE
            }
        },
        Some("--help" | "-h" | "help") | None => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command `{other}`\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
