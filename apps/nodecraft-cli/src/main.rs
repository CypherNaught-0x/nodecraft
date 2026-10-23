//! `nodecraft-cli`: the headless NodeCraft frontend.
//!
//! `mcp` serves the engine's commands to MCP clients over stdio. `commands`, `inspect`, `quote`
//! and `providers` read projects and the engine without changing them; `run` runs a project's
//! nodes and exports their results, leaving the project file as it was. `serve` offers a
//! project's techniques as a local HTTP API, and `webhook-secret` keeps the secret its webhooks are
//! signed with. `relay` passes projects shared live between peers. `batch` and `comfy` arrive with their milestones
//! (docs/plan.md §4.8).

mod control;
mod inspect;
mod project;
mod providers;
mod quote;
mod relay;
mod run;
mod serve;

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
                  price a run of the nodes and their upstream (default: every stale or never-run node);
                  nothing runs and nothing is charged
  run <project.ncraft> [--canvas <id>] [--node <id>]... [--out <folder>] [--yes]
                  run the nodes and their stale upstream (default: every stale or never-run node)
                  and export their media results into the folder; a run that costs money, or
                  whose price isn't fully known, needs --yes (check it with `quote` first). The
                  project file is not changed. Stopping the CLI mid-run doesn't stop jobs a
                  provider already accepted; they may still be charged
  serve <project.ncraft> [--listen <addr>] [--token-file <path>] [--max-cost <CUR=amount>]...
        [--allow-unpriced] [--webhook-allow <url>]... [--webhook-allow-private]
                  offer the project's published techniques as a local HTTP API (default
                  127.0.0.1:7341); requests need the bearer token written to the token file
                  (default control/serve.json in the app's settings folder). A run may spend
                  only up to --max-cost per currency (nothing without it), and a run whose price
                  isn't fully known needs --allow-unpriced. A run may name a webhook under a
                  --webhook-allow URL, called when it ends and signed with the webhook secret;
                  private addresses need --webhook-allow-private. The project file is not changed
  relay [--listen <addr>]
                  pass projects shared live between the people working on them (default
                  127.0.0.1:7342). Each project gets a room whose random id is its key; the
                  relay keeps nothing on disk. It speaks plain ws://, so put it behind a TLS
                  proxy before people reach it over the internet
  webhook-secret [--delete]
                  keep the secret `serve` signs webhooks with in the keychain, read from stdin
                  (at least 32 characters; `whsec_<base64>` is decoded); --delete removes it
  providers       list the model providers and whether each can run (reads the keychain)
  --version, -V   print the version, commit and build date
  --help, -h      print this help

Add --json to commands, inspect, quote, run and providers for machine-readable output.
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
    // The app's Elements library (#552), for agents to list and place from.
    if let Some(dir) = nodecraft_engine::settings::settings_dir() {
        nodecraft_engine::element_library::install(&mut session, std::rc::Rc::new(nodecraft_engine::element_library::FileElements::new(dir.join("elements"))));
    }
    // The person's agent skills (#168), shared with the app; the folder is made on the first change.
    if let Some(dir) = nodecraft_engine::settings::settings_dir()
        && let Err(e) = nodecraft_engine::skills::install(&mut session, std::rc::Rc::new(nodecraft_engine::skills::FileSkills::new(dir.join("skills"))))
    {
        eprintln!("warning: your skills are unavailable: {}", e.message);
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
    out: Option<String>,
    yes: bool,
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
            "--out" if out.out.is_some() => return Err("`--out` is given twice".into()),
            "--out" => out.out = Some(value()?),
            "--yes" => out.yes = true,
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
    let out = if json {
        match serde_json::to_string_pretty(value) {
            Ok(out) => out + "\n",
            Err(error) => return fail(&format!("can't write JSON: {error}")),
        }
    } else {
        text(value)
    };
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
        "run" => &["--json", "--canvas", "--node", "--out", "--yes"],
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
            if command == "run" {
                return run(&path, &options);
            }
            let mut session = match project::open(&path) {
                Ok(session) => session,
                Err(error) => return fail(&error.message),
            };
            let session = &mut session;
            if command == "inspect" {
                return match inspect::inspect(session) {
                    Ok(report) => print(options.json, &report, inspect::inspect_text),
                    Err(error) => fail(&error.message),
                };
            }
            let quoted = quote::canvas_of(session, options.canvas.as_deref()).and_then(|canvas| {
                let targets = quote::targets(session, canvas, &options.nodes)?;
                let mut maker = providers::keychain_maker(nodecraft_engine::settings::peek().providers)?;
                quote::prepare(session, canvas, &targets, &mut maker)?;
                quote::quote(session, canvas, &targets).map(|quote| (canvas, quote))
            });
            match quoted {
                Ok((canvas, value)) => print(options.json, &value, |value| quote::quote_text(session, canvas, value)),
                Err(error) => fail(&error.message),
            }
        }
    }
}

/// `serve`: offers the project's techniques until the process is stopped.
fn serve_command(args: &[String]) -> ExitCode {
    let mut path = None;
    let mut config =
        serve::Config { listen: std::net::SocketAddr::from(([127, 0, 0, 1], 7341)), token_file: None, policy: serve::Policy::default(), webhooks: None };
    let (mut allow, mut allow_private) = (Vec::new(), false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("`{flag}` needs a value"));
        let parsed: Result<(), String> = match arg.as_str() {
            "--listen" => value("--listen")
                .and_then(|v| v.parse().map(|addr| config.listen = addr).map_err(|_| format!("`--listen` takes an address like 127.0.0.1:7341, not `{v}`"))),
            "--token-file" => value("--token-file").map(|v| config.token_file = Some(v.into())),
            "--max-cost" => value("--max-cost").and_then(|v| serve::parse_max_cost(v)).map(|(code, micros)| {
                config.policy.max_cost.insert(code, micros);
            }),
            "--allow-unpriced" => {
                config.policy.allow_unpriced = true;
                Ok(())
            }
            "--webhook-allow" => value("--webhook-allow").and_then(|v| serve::parse_webhook_allow(v)).map(|url| allow.push(url)),
            "--webhook-allow-private" => {
                allow_private = true;
                Ok(())
            }
            flag if flag.starts_with("--") => Err(format!("`serve` doesn't take `{flag}`")),
            project if path.is_none() => {
                path = Some(std::path::PathBuf::from(project));
                Ok(())
            }
            _ => Err("`serve` takes one project file".into()),
        };
        if let Err(message) = parsed {
            return fail(&format!("{message}\n\n{USAGE}"));
        }
    }
    let Some(path) = path else { return fail(&format!("`serve` needs a project file\n\n{USAGE}")) };
    if allow.is_empty() && allow_private {
        return fail("`--webhook-allow-private` needs at least one `--webhook-allow`");
    }
    if !allow.is_empty() {
        // Checked now, so a missing secret shows before the first run rather than in its webhook.
        use nodecraft_engine::secrets::SecretStore;
        let store = Arc::new(KeyringStore);
        let present = nodecraft_engine::secrets::Account::new(serve::WEBHOOK_ACCOUNT).and_then(|account| store.get(&account));
        match present {
            Ok(Some(_)) => {}
            Ok(None) => return fail("webhooks need a signing secret: run `nodecraft-cli webhook-secret` first"),
            Err(error) => return fail(&format!("can't read the webhook secret: {error}")),
        }
        config.webhooks = Some(serve::Webhooks::new(allow, allow_private, store));
    }
    match serve::start(config, move || serve::load(&path)) {
        Ok(server) => {
            let _ = writeln!(std::io::stderr(), "Serving on http://{}/ (token in {}). Stop with Ctrl-C.", server.addr, server.token_file.display());
            server.wait();
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.message),
    }
}

/// `relay [--listen <addr>]`: passes shared projects between peers until the process is stopped.
fn relay_command(args: &[String]) -> ExitCode {
    let listen = match args {
        [] => Ok(std::net::SocketAddr::from(([127, 0, 0, 1], 7342))),
        [flag, addr] if flag == "--listen" => addr.parse().map_err(|_| format!("`--listen` takes an address like 127.0.0.1:7342, not `{addr}`")),
        _ => Err("`relay` takes only `--listen <addr>`".into()),
    };
    let listen = match listen {
        Ok(listen) => listen,
        Err(message) => return fail(&format!("{message}\n\n{USAGE}")),
    };
    match relay::start(listen) {
        Ok(relay) => {
            let _ = writeln!(std::io::stderr(), "Relaying on ws://{}/. Stop with Ctrl-C.", relay.addr);
            relay.wait();
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.message),
    }
}

/// `webhook-secret [--delete]`: keeps `serve`'s webhook signing secret, read from stdin, in the
/// keychain, or removes it. The secret is never printed.
fn webhook_secret(args: &[String]) -> ExitCode {
    use nodecraft_engine::secrets::{Account, Secret, SecretStore};
    use std::io::Read;
    let account = match Account::new(serve::WEBHOOK_ACCOUNT) {
        Ok(account) => account,
        Err(error) => return fail(&error.to_string()),
    };
    match args {
        [] => {}
        [flag] if flag == "--delete" => {
            return match KeyringStore.delete(&account) {
                Ok(true) => {
                    let _ = writeln!(std::io::stderr(), "Removed the webhook secret.");
                    ExitCode::SUCCESS
                }
                Ok(false) => {
                    let _ = writeln!(std::io::stderr(), "No webhook secret was set.");
                    ExitCode::SUCCESS
                }
                Err(error) => fail(&error.to_string()),
            };
        }
        _ => return fail(&format!("`webhook-secret` takes only `--delete`\n\n{USAGE}")),
    }
    let mut text = String::new();
    // A little more than the longest secret, so an over-long one is refused rather than cut.
    if std::io::stdin().take(4096).read_to_string(&mut text).is_err() {
        return fail("the secret on stdin isn't text");
    }
    let secret = match Secret::new(&text) {
        Ok(secret) => secret,
        Err(error) => return fail(&error.to_string()),
    };
    if let Err(error) = serve::webhook_key(secret.expose()) {
        return fail(&error);
    }
    match KeyringStore.set(&account, &secret) {
        Ok(()) => {
            let _ = writeln!(std::io::stderr(), "Saved the webhook secret in the keychain; running servers use it from their next delivery.");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error.to_string()),
    }
}

/// `run`: quotes, runs once the price is agreed to, then exports the results.
fn run(path: &std::path::Path, options: &Options) -> ExitCode {
    let mut new_id = random_bytes();
    let mut services = Services::default();
    let session_id: String = new_id().iter().take(8).map(|b| format!("{b:02x}")).collect();
    let mut runner = match run::Runner::new(&mut services, session_id) {
        Ok(runner) => runner,
        Err(error) => return fail(&error.message),
    };
    let mut session = match project::open_with_assets(path) {
        Ok(session) => session.with_services(services),
        Err(error) => return fail(&error.message),
    };
    let session = &mut session;
    // Checked before anything runs, so a typo costs nothing.
    let out = match options.out.as_deref().map(|out| run::out_folder(std::path::Path::new(out))).transpose() {
        Ok(out) => out,
        Err(error) => return fail(&error.message),
    };
    let done = quote::canvas_of(session, options.canvas.as_deref()).and_then(|canvas| {
        let targets = quote::targets(session, canvas, &options.nodes)?;
        let mut maker = providers::keychain_maker(nodecraft_engine::settings::peek().providers)?;
        quote::prepare(session, canvas, &targets, &mut maker)?;
        runner.install(session);
        let quoted = quote::quote(session, canvas, &targets)?;
        let steps = runner.run(session, canvas, &targets, &quoted, run::Consent { spend: options.yes })?;
        // An unaccounted result was kept on its card, so it is exported too.
        let kept = |s: &&serde_json::Value| s["status"] == "recorded" || s["status"] == "unaccounted";
        let ran: Vec<NodeId> = steps.iter().filter(kept).filter_map(|s| s["node"].as_str()?.parse().ok()).collect();
        let results = match &out {
            Some(out) => run::export(session, canvas, &ran, out)?,
            None => Vec::new(),
        };
        Ok(serde_json::json!({"quote": quoted, "steps": steps, "results": results}))
    });
    match done {
        Ok(report) => {
            let listed = |key: &str| report[key].as_array().into_iter().flatten().cloned().collect::<Vec<_>>();
            let failed = listed("steps").iter().any(|step| step["status"] != "recorded") || listed("results").iter().any(|r| r["status"] == "failed");
            let printed = print(options.json, &report, run::report_text);
            if failed { ExitCode::FAILURE } else { printed }
        }
        Err(error) => fail(&error.message),
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
        Some("serve") => serve_command(args.get(1..).unwrap_or_default()),
        Some("relay") => relay_command(args.get(1..).unwrap_or_default()),
        Some("webhook-secret") => webhook_secret(args.get(1..).unwrap_or_default()),
        Some(command @ ("commands" | "inspect" | "quote" | "run" | "providers")) => read_only(command, args.get(1..).unwrap_or_default()),
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
