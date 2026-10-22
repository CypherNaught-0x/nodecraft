//! Workspace tooling: `cargo xtask <command>`.
//!
//! Pure Rust (std + serde_json). External tools (`cargo`) are invoked through
//! `std::process::Command`.

mod decisions;
mod layers;
mod perf;
mod test_layout;
mod update_feed;
mod version;
mod workspace;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  perf            bounded release collect/compare tooling (cargo xtask perf help)
  layers          enforce the crate dependency layering (docs/architecture.md), and that every
                  integration test file is built
  wasm            cargo check --target wasm32-unknown-unknown for the wasm-safe crates
  catalog-check   validate assets/catalog/*.toml and that each file is built in (docs/catalog.md)
  decisions       check docs/decisions/: one dated file per decision, none in decisions.md
  ci              fmt --check, clippy -D warnings, test, layers, wasm (stops at first failure;
                  `test` includes catalog-check, `layers` includes decisions)
  version [set X.Y.Z[-pre]]
                  print the workspace version, or set it (Cargo.toml + Cargo.lock)
  update-key <secret file>
                  make an update signing key pair: the public key goes in packaging/update-key.pub
  update-feed <artifact folder> <version>
                  write and sign the release's update feed (NODECRAFT_UPDATE_SIGNING_KEY)
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
    let result = match args.first().map(String::as_str) {
        Some("perf") => {
            return match root().and_then(|root| perf::run(&root, &rest)) {
                Ok(code) => ExitCode::from(code),
                Err(error) => {
                    eprintln!("error: {error}");
                    ExitCode::from(3)
                }
            };
        }
        Some(command @ ("layers" | "wasm" | "ci" | "catalog-check" | "decisions" | "version" | "update-key" | "update-feed")) => root().and_then(|root| {
            eprintln!("xtask workspace: {}", root.display());
            match command {
                "layers" => cmd_layers(&root),
                "wasm" => cmd_wasm(&root),
                "ci" => cmd_ci(&root),
                "catalog-check" => cmd_catalog_check(&root),
                "decisions" => decisions::check(&root),
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
    c
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    eprintln!("$ {what}");
    let status = cmd.status().map_err(|e| format!("{what}: failed to spawn: {e}"))?;
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
    c.args([
        "check",
        "--target",
        "wasm32-unknown-unknown",
        "--features",
        "nodecraft-providers/openai-images,nodecraft-providers/replicate,nodecraft-providers/seedream,nodecraft-providers/gemini-images,nodecraft-providers/gemini-video,nodecraft-providers/google-veo,nodecraft-providers/google-tts,nodecraft-providers/seedance,nodecraft-providers/fal,nodecraft-providers/tripo,nodecraft-providers/meshy,nodecraft-providers/higgsfield,nodecraft-providers/kling,nodecraft-providers/elevenlabs,nodecraft-providers/comfy-cloud",
    ]);
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

fn cmd_ci(root: &Path) -> Result<(), String> {
    type Step<'a> = (&'static str, Box<dyn Fn() -> Result<(), String> + 'a>);
    let cargo_step = |args: &'static [&'static str]| -> Box<dyn Fn() -> Result<(), String> + '_> {
        Box::new(move || {
            let mut c = cargo(root);
            c.args(args);
            run(c, &format!("cargo {}", args.join(" ")))
        })
    };
    let steps: Vec<Step<'_>> = vec![
        ("fmt", cargo_step(&["fmt", "--all", "--", "--check"])),
        (
            "clippy",
            cargo_step(&[
                "clippy",
                "--workspace",
                "--all-targets",
                "--features",
                "nodecraft-providers/openai-images,nodecraft-providers/replicate,nodecraft-providers/seedream,nodecraft-providers/gemini-images,nodecraft-providers/gemini-video,nodecraft-providers/google-veo,nodecraft-providers/google-tts,nodecraft-providers/seedance,nodecraft-providers/fal,nodecraft-providers/tripo,nodecraft-providers/meshy,nodecraft-providers/higgsfield,nodecraft-providers/kling,nodecraft-providers/elevenlabs,nodecraft-providers/comfy-cloud",
                "--",
                "-D",
                "warnings",
            ]),
        ),
        (
            "test",
            cargo_step(&[
                "test",
                "--workspace",
                "--features",
                "nodecraft-providers/openai-images,nodecraft-providers/replicate,nodecraft-providers/seedream,nodecraft-providers/gemini-images,nodecraft-providers/gemini-video,nodecraft-providers/google-veo,nodecraft-providers/google-tts,nodecraft-providers/seedance,nodecraft-providers/fal,nodecraft-providers/tripo,nodecraft-providers/meshy,nodecraft-providers/higgsfield,nodecraft-providers/kling,nodecraft-providers/elevenlabs,nodecraft-providers/comfy-cloud",
            ]),
        ),
        ("layers", Box::new(|| cmd_layers(root).and_then(|()| decisions::check(root)))),
        ("wasm", Box::new(|| cmd_wasm(root))),
    ];
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
    Ok(())
}
