//! `nodecraft-cli`: the headless NodeCraft frontend.
//!
//! `mcp` serves the engine's commands to MCP clients over stdio. `commands`, `inspect`, `quote`
//! and `providers` read projects and the engine without changing them. Running graphs and
//! techniques, `batch`, `comfy` and `serve` arrive with their milestones (docs/plan.md §4.8).

mod control;
mod inspect;
mod project;
mod providers;
mod quote;

use std::hash::{BuildHasher, RandomState};
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use nodecraft_automation::mcp::{McpServer, ServeEnd, serve};
use nodecraft_engine::commands::Services;
use nodecraft_engine::secrets::{KeyringStore, register_key_service};
use nodecraft_types::{CanvasId, NodeId, ProjectId};

const USAGE: &str = "\
usage: nodecraft-cli <command>

commands:
  control         serve headless control JSON over stdin/stdout (a new, empty project)
  mcp             serve NodeCraft to an MCP client over stdin/stdout (a new, empty project)
  mcp --bridge [token-file]
                  serve the running app instead (started with `nodecraft --control`); the
                  token file defaults to control/control.json in the app's settings folder
  commands [--full]
                  list the engine's commands; --full adds their parameters
  inspect <project.ncraft>
                  list a project's canvases and nodes, with which are stale
  quote <project.ncraft> [--canvas <id>] [--node <id>]...
                  price a run of the nodes and their upstream (default: every stale node);
                  nothing runs and nothing is charged
  providers       list the model providers and whether each can run (reads the keychain)
  --version, -V   print the version, commit and build date
  --help, -h      print this help

Add --json to commands, inspect, quote and providers for machine-readable output.
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
    // Resolved only: the CLI must not create, probe or repair the app's settings folder.
    nodecraft_engine::settings::settings_dir().map(|dir| dir.join("control").join("control.json"))
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
    let mut session = match session.and_then(|s| register_key_service(&mut services, Arc::new(KeyringStore)).map(|()| s)) {
        Ok(s) => s.with_services(services),
        Err(e) => {
            eprintln!("error: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    // The app's technique library (#96), for agents to list and place from. Agents never change
    // it, so the CLI only reads the file; nothing is created when the app has no library yet.
    if let Some(dir) = nodecraft_engine::settings::settings_dir()
        && let Err(e) = nodecraft_engine::library::install(&mut session, Box::new(nodecraft_engine::library::FileLibrary::new(dir.join("library"))))
    {
        eprintln!("warning: the technique library is unavailable: {}", e.message);
    }
    serve_stdio(&mut McpServer::new(session, Box::new(new_id)))
}

/// The options of the read-only commands.
#[derive(Default)]
struct Options {
    json: bool,
    full: bool,
    canvas: Option<String>,
    nodes: Vec<NodeId>,
    positional: Vec<String>,
}

/// Reads `args`, refusing a flag not in `allowed`.
fn options(args: &[String], allowed: &[&str]) -> Result<Options, String> {
    let mut out = Options::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let flag = arg.as_str();
        if flag.starts_with("--") && !allowed.contains(&flag) {
            return Err(format!("unknown option `{flag}`"));
        }
        let mut value = || rest.next().cloned().ok_or_else(|| format!("`{flag}` needs a value"));
        match flag {
            "--json" => out.json = true,
            "--full" => out.full = true,
            "--canvas" if out.canvas.is_some() => return Err("`--canvas` is given twice".into()),
            "--canvas" => out.canvas = Some(value()?),
            "--node" => {
                let id = value()?;
                out.nodes.push(id.parse().map_err(|_| format!("`{id}` isn't a node id (see `inspect`)"))?);
            }
            _ => out.positional.push(arg.clone()),
        }
    }
    Ok(out)
}

/// Prints `value` as JSON or, for people, as `text` renders it.
fn print(json: bool, value: &serde_json::Value, text: impl FnOnce(&serde_json::Value) -> String) -> ExitCode {
    let out = if json { serde_json::to_string_pretty(value).map(|mut s| (s.push('\n'), s).1).unwrap_or_default() } else { text(value) };
    // A closed pipe (`| head`) isn't an error worth a message.
    let _ = std::io::stdout().lock().write_all(out.as_bytes());
    ExitCode::SUCCESS
}

fn fail(message: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "error: {message}");
    ExitCode::FAILURE
}

/// The one project path a command takes.
fn project_arg(command: &str, options: &Options) -> Result<std::path::PathBuf, String> {
    match options.positional.as_slice() {
        [path] => Ok(std::path::PathBuf::from(path)),
        [] => Err(format!("`{command}` needs a project file")),
        _ => Err(format!("`{command}` takes one project file")),
    }
}

fn read_only(command: &str, args: &[String]) -> ExitCode {
    let allowed: &[&str] = match command {
        "commands" => &["--json", "--full"],
        "quote" => &["--json", "--canvas", "--node"],
        _ => &["--json"],
    };
    let options = match options(args, allowed) {
        Ok(options) => options,
        Err(message) => return fail(&format!("{message}\n\n{USAGE}")),
    };
    let mut new_id = random_bytes();
    match command {
        "commands" | "providers" => {
            if !options.positional.is_empty() {
                return fail(&format!("`{command}` takes no arguments\n\n{USAGE}"));
            }
            let session = match nodecraft_automation::blank_session(ProjectId::from_bytes(new_id()), CanvasId::from_bytes(new_id())) {
                Ok(session) => session,
                Err(error) => return fail(&error.message),
            };
            if command == "commands" {
                return print(options.json, &inspect::commands(&session, options.full), |listed| inspect::commands_text(listed, options.full));
            }
            let listed = providers::keychain_maker(nodecraft_engine::settings::peek().providers).and_then(|maker| providers::providers(&session, maker));
            match listed {
                Ok(listed) => print(options.json, &listed, providers::providers_text),
                Err(error) => fail(&error.message),
            }
        }
        _ => {
            let path = match project_arg(command, &options) {
                Ok(path) => path,
                Err(message) => return fail(&format!("{message}\n\n{USAGE}")),
            };
            let mut opened = match project::open(&path, &mut new_id) {
                Ok(opened) => opened,
                Err(error) => return fail(&error.message),
            };
            let session = &mut opened.session;
            if command == "inspect" {
                return match inspect::inspect(session) {
                    Ok(report) => print(options.json, &report, inspect::inspect_text),
                    Err(error) => fail(&error.message),
                };
            }
            let quoted = quote::canvas_of(session, options.canvas.as_deref()).and_then(|canvas| {
                let mut maker = providers::keychain_maker(nodecraft_engine::settings::peek().providers)?;
                quote::prepare(session, canvas, &options.nodes, &mut maker)?;
                quote::quote(session, canvas, &options.nodes).map(|quote| (canvas, quote))
            });
            match quoted {
                Ok((canvas, value)) => print(options.json, &value, |value| quote::quote_text(session, canvas, value)),
                Err(error) => fail(&error.message),
            }
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
        Some("control") if args.len() == 1 => control::run(),
        Some("control") => {
            let _ = writeln!(std::io::stderr(), "error: `control` takes no options\n\n{USAGE}");
            ExitCode::FAILURE
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
        Some(command @ ("commands" | "inspect" | "quote" | "providers")) => read_only(command, args.get(1..).unwrap_or_default()),
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
