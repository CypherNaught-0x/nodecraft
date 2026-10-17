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
    let mut server = McpServer::new(session, Box::new(new_id));
    // Stdout carries the protocol only; diagnostics go to stderr.
    match serve(&mut server, &mut std::io::stdin().lock(), &mut std::io::stdout().lock()) {
        ServeEnd::Closed => ExitCode::SUCCESS,
        ServeEnd::Io(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
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
        Some("mcp") => mcp(),
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
