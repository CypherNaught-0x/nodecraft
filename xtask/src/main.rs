//! Workspace tooling: `cargo xtask <command>`.
//!
//! Pure Rust (std + serde_json). External tools (`cargo`) are invoked through
//! `std::process::Command`.

mod decisions;
mod layers;
mod perf;
mod shared_target;
mod test_layout;
mod test_step;
mod update_feed;
mod version;
mod workspace;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Every optional provider adapter: clippy, tests and the wasm check build them all.
const PROVIDER_FEATURES: &str = "nodecraft-providers/openai-audio,nodecraft-providers/openai-images,nodecraft-providers/replicate,nodecraft-providers/runway,nodecraft-providers/seedream,nodecraft-providers/bfl,nodecraft-providers/gemini-images,nodecraft-providers/gemini-llm,nodecraft-providers/huggingface,nodecraft-providers/gemini-video,nodecraft-providers/google-veo,nodecraft-providers/google-tts,nodecraft-providers/google-gemini-images,nodecraft-providers/seedance,nodecraft-providers/fal,nodecraft-providers/tripo,nodecraft-providers/meshy,nodecraft-providers/higgsfield,nodecraft-providers/kling,nodecraft-providers/elevenlabs,nodecraft-providers/deepgram,nodecraft-providers/stability,nodecraft-providers/magnific,nodecraft-providers/comfy-cloud,nodecraft-providers/openrouter-images,nodecraft-providers/openrouter-media";

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  perf            bounded release collect/compare tooling (cargo xtask perf help)
  layers          enforce the crate dependency layering (docs/architecture.md), and that every
                  integration test file is built
  wasm            cargo check --target wasm32-unknown-unknown for the wasm-safe crates
  catalog-check   validate assets/catalog/*.toml and that each file is built in (docs/catalog.md)
  decisions       check docs/decisions/: one dated file per decision, none in decisions.md
  ci [--quick [--base REF]]
                  fmt --check, clippy -D warnings, test, layers, wasm (stops at first failure;
                  `test` includes catalog-check, `layers` includes decisions). Run it in full
                  before a pull request. `--quick` is for iterating: it tests only the packages
                  changed since REF (default origin/main) and their dependents, leaves out the
                  slow tests and doctests, and skips wasm. Uses cargo-nextest when installed;
                  `--quick` requires it
  slow-tests [--threshold SECS]
                  time every test and regenerate the slow-test list in .config/nextest.toml
                  (default 5 seconds)
  shared -- <cargo args>
                  run an explicit Cargo command in the shared build directory (needs
                  NODECRAFT_SHARED_TARGET). Use `shared --clean` to remove an owned cache.
  version [set X.Y.Z[-pre]]
                  print the workspace version, or set it (Cargo.toml + Cargo.lock)
  update-key <secret file>
                  make an update signing key pair: the public key goes in packaging/update-key.pub
  update-feed <artifact folder> <version>
                  write and sign the release's update feed (NODECRAFT_UPDATE_SIGNING_KEY)

NODECRAFT_SHARED_TARGET=<absolute dir> makes ci, wasm, catalog-check, slow-tests and shared
build in one directory shared by every checkout: one build at a time, and the workspace
crates rebuilt whenever a different checkout builds (docs/development.md, \"Sharing one build
directory\").
";

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
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    // Held until main returns: the shared build directory stays locked for the whole command.
    let shared = match args.first().map(String::as_str) {
        Some("ci" | "wasm" | "catalog-check" | "slow-tests") => match share() {
            Ok(guard) => guard,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    let result = match args.first().map(String::as_str) {
        Some("shared") => root().and_then(|root| cmd_shared(&root, &rest)),
        Some("perf") => {
            if rest.first() == Some(&"collect") && shared_target::inherited() {
                eprintln!("error: perf collect needs an isolated target. Run it outside a shared command.");
                return ExitCode::from(3);
            }
            return match root().and_then(|root| perf::run(&root, &rest)) {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(3)
                }
            };
        }
        Some(command @ ("layers" | "wasm" | "ci" | "catalog-check" | "decisions" | "slow-tests" | "version" | "update-key" | "update-feed")) => root()
            .and_then(|root| {
                eprintln!("xtask workspace: {}", root.display());
                match command {
                    "layers" => cmd_layers(&root),
                    "wasm" => cmd_wasm(&root),
                    "ci" => cmd_ci(&root, &rest),
                    "catalog-check" => cmd_catalog_check(&root),
                    "decisions" => decisions::check(&root),
                    "slow-tests" => test_step::slow_tests(&root, &rest),
                    "update-key" => update_feed::key(&root, &rest),
                    "update-feed" => update_feed::feed(&root, &rest),
                    _ => version::run(&root, &rest),
                }
            }),
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    let result = match shared {
        Some(guard) => guard.finish(result),
        None => result,
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve the invoking workspace at runtime; never trust a compiled-in checkout.
pub fn root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("read invocation directory: {e}"))?;
    workspace::resolve(&cwd)
}

pub fn cargo(root: &Path) -> Command {
    let mut c = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(root);
    shared_target::configure(&mut c);
    shared_target::cargo_options(&mut c, root);
    c
}

fn cargo_task(root: &Path, subcommand: &str, args: &[&str]) -> Command {
    let Some(executable) = shared_target::extension(subcommand) else {
        let mut command = cargo(root);
        command.arg(subcommand).args(args);
        return command;
    };
    let mut command = Command::new(executable);
    command.current_dir(root).arg(subcommand);
    shared_target::configure(&mut command);
    match (subcommand, args) {
        ("clippy", ["--version" | "-V" | "--help" | "-h"]) => {
            command.args(args);
        }
        ("clippy", _) => {
            shared_target::cargo_options(&mut command, root);
            command.args(args);
        }
        ("nextest", [operation @ ("run" | "list" | "archive"), rest @ ..]) => {
            command.arg(operation);
            shared_target::cargo_options(&mut command, root);
            command.args(rest);
        }
        _ => {
            command.args(args);
        }
    }
    command
}

/// Locks and routes into the shared build directory when `NODECRAFT_SHARED_TARGET` is set.
fn share() -> Result<Option<shared_target::Guard>, String> {
    let Some(shared) = shared_target::configured()? else { return Ok(None) };
    let root = root()?;
    shared_target::acquire(&root, &shared, &member_names(&root)?).map(Some)
}

/// The workspace's own package names: the artifacts a shared directory must never reuse across
/// checkouts.
fn member_names(root: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    let meta = metadata(root)?;
    let packages = meta.get("packages").and_then(serde_json::Value::as_array).ok_or("cargo metadata omitted packages")?;
    packages
        .iter()
        .map(|p| p.get("name").and_then(serde_json::Value::as_str).map(String::from).ok_or_else(|| "cargo metadata omitted a package name".to_string()))
        .collect()
}

/// `cargo xtask shared -- <cargo args>` and `cargo xtask shared --clean`.
fn cmd_shared(root: &Path, args: &[&str]) -> Result<(), String> {
    const USAGE: &str = "usage: cargo xtask shared -- <cargo args> | cargo xtask shared --clean";
    if shared_target::inherited() {
        return Err("shared needs a separate invocation outside the managed command".into());
    }
    let shared = shared_target::configured()?.ok_or_else(|| format!("{} is not set\n{USAGE}", shared_target::ENV))?;
    match args {
        ["--clean"] => shared_target::clean(root, &shared),
        ["--", subcommand, cargo_args @ ..] => {
            if let Some(why) = shared_target::refused_argument(subcommand, cargo_args) {
                return Err(format!("refused: {why}"));
            }
            let guard = shared_target::acquire(root, &shared, &member_names(root)?)?;
            let c = cargo_task(root, subcommand, cargo_args);
            let result = run(c, &format!("cargo {subcommand} {}", cargo_args.join(" ")));
            guard.finish(result)
        }
        _ => Err(USAGE.into()),
    }
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    eprintln!("$ {what}");
    let mut child = cmd.spawn().map_err(|e| format!("{what}: failed to spawn: {e}"))?;
    let status = child.wait().map_err(|e| {
        shared_target::interrupted_child();
        format!("{what}: failed to wait for child: {e}")
    })?;
    // On Windows an externally terminated process can report an ordinary nonzero code.
    #[cfg(windows)]
    let uncertain = !status.success();
    #[cfg(not(windows))]
    let uncertain = status.code().is_none();
    if uncertain {
        shared_target::interrupted_child();
    }
    if status.success() { Ok(()) } else { Err(format!("{what}: exited with {status}")) }
}

fn metadata(root: &Path) -> Result<serde_json::Value, String> {
    let out = cargo(root).args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))
}

fn cmd_layers(root: &Path) -> Result<(), String> {
    let crates = layers::from_metadata(&metadata(root)?)?;
    println!("Dependency layering (docs/architecture.md)\n");
    println!("{:<24} {:<14} workspace deps", "crate", "layer");
    for c in &crates {
        let ws: Vec<String> = c
            .deps
            .iter()
            .filter(|d| d.workspace)
            .map(|d| {
                let k = match d.kind {
                    layers::DepKind::Normal => "",
                    layers::DepKind::Dev => " (dev)",
                    layers::DepKind::Build => " (build)",
                };
                format!("{}{k}", layers::short_name(&d.name))
            })
            .collect();
        println!("{:<24} {:<14} {}", c.name, layers::describe(layers::classify(&c.name)), ws.join(", "));
    }
    let violations = layers::check(&crates);
    println!();
    if violations.is_empty() {
        println!("OK: {} crates, no layering violations.", crates.len());
        test_layout::check(root)
    } else {
        println!("{} violation(s):", violations.len());
        for v in &violations {
            println!("  - {v}");
        }
        Err(format!("{} layering violation(s)", violations.len()))
    }
}

/// Workspace packages that must build for wasm32: every layered library crate, the egui shell
/// included. Test tooling, apps and xtask are not checked.
fn wasm_set(root: &Path) -> Result<Vec<String>, String> {
    let crates = layers::from_metadata(&metadata(root)?)?;
    Ok(crates.into_iter().filter(|c| matches!(layers::classify(&c.name), Some(layers::Class::Layer(_) | layers::Class::Standalone))).map(|c| c.name).collect())
}

fn cmd_wasm(root: &Path) -> Result<(), String> {
    // Keep engine's transitive browser types valid without UI feature unification.
    let mut isolated = cargo(root);
    isolated.args(["check", "-p", "nodecraft-automation", "--lib", "--target", "wasm32-unknown-unknown"]);
    run(isolated, "isolated automation/engine wasm check (without UI)")?;
    let set = wasm_set(root)?;
    let mut c = cargo(root);
    c.args(["check", "--target", "wasm32-unknown-unknown", "--features", PROVIDER_FEATURES]);
    for pkg in &set {
        c.args(["-p", pkg]);
    }
    run(c, &format!("cargo check --target wasm32-unknown-unknown ({} crates)", set.len()))?;
    // Test tooling is intentionally outside the layered library set. Exercise the facade
    // and opt-in implementation explicitly without changing shipping feature defaults.
    let mut mocks = cargo(root);
    mocks.args([
        "check",
        "--target",
        "wasm32-unknown-unknown",
        "-p",
        "nodecraft-provider-testkit",
        "-p",
        "nodecraft-providers",
        "--features",
        "nodecraft-providers/testkit",
    ]);
    run(mocks, "cargo check --target wasm32-unknown-unknown -p nodecraft-provider-testkit -p nodecraft-providers --features nodecraft-providers/testkit")?;
    // The browser app is an app, not a layered crate, but wasm is the only target it has (#99).
    let mut web = cargo(root);
    web.args(["check", "--target", "wasm32-unknown-unknown", "-p", "nodecraft-web"]);
    run(web, "cargo check --target wasm32-unknown-unknown -p nodecraft-web")?;
    println!("\nwasm32-unknown-unknown check: ok for {}", set.join(", "));
    println!("wasm32-unknown-unknown opt-in check: ok for nodecraft-provider-testkit and nodecraft-providers/testkit");
    println!("wasm32-unknown-unknown app check: ok for nodecraft-web");
    Ok(())
}

/// Every catalog file parses, validates, merges without duplicate ids, and is embedded. The
/// check itself is `crates/catalog/tests/catalog_files.rs`, so xtask stays std-only and the
/// same check runs in `cargo test`.
fn cmd_catalog_check(root: &Path) -> Result<(), String> {
    let mut c = cargo(root);
    c.args(["test", "-p", "nodecraft-catalog", "--test", "catalog_files", "--", "--nocapture"]);
    run(c, "cargo test -p nodecraft-catalog --test catalog_files")
}

fn cmd_ci(root: &Path, args: &[&str]) -> Result<(), String> {
    let quick = match args {
        [] => None,
        ["--quick"] => Some("origin/main"),
        ["--quick", "--base", base] => Some(*base),
        _ => return Err("usage: cargo xtask ci [--quick [--base REF]]".into()),
    };
    type Step<'a> = (&'static str, Box<dyn Fn() -> Result<(), String> + 'a>);
    let cargo_step = |command: &'static str, args: &'static [&'static str]| -> Box<dyn Fn() -> Result<(), String> + '_> {
        Box::new(move || {
            let c = cargo_task(root, command, args);
            run(c, &format!("cargo {command} {}", args.join(" ")))
        })
    };
    let mut steps: Vec<Step<'_>> = vec![
        ("fmt", cargo_step("fmt", &["--all", "--", "--check"])),
        ("clippy", cargo_step("clippy", &["--workspace", "--all-targets", "--features", PROVIDER_FEATURES, "--", "-D", "warnings"])),
        (
            "test",
            match quick {
                Some(base) => Box::new(move || test_step::quick(root, base)),
                None => Box::new(|| test_step::full(root)),
            },
        ),
        ("layers", Box::new(|| cmd_layers(root).and_then(|()| decisions::check(root)))),
    ];
    if quick.is_none() {
        steps.push(("wasm", Box::new(|| cmd_wasm(root))));
    }
    let mut done = Vec::new();
    for (name, f) in &steps {
        eprintln!("\n=== ci: {name} ===");
        if let Err(e) = f() {
            println!("\nCI summary:");
            for d in &done {
                println!("  ok    {d}");
            }
            println!("  FAIL  {name}: {e}");
            for (n, _) in steps.iter().skip(done.len() + 1) {
                println!("  skip  {n}");
            }
            return Err(format!("ci failed at `{name}`"));
        }
        done.push(*name);
    }
    println!("\nCI summary: all {} steps passed ({})", done.len(), done.join(", "));
    if quick.is_some() {
        println!("Quick: unaffected packages, slow tests, doctests and wasm were not checked. Run `cargo xtask ci` before a pull request.");
    }
    Ok(())
}
