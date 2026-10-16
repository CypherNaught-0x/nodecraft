//! `nodecraft-cli`: the headless NodeCraft frontend.
//!
//! M0 only reports its version. Running graphs and techniques, `quote`, `inspect`, `commands`,
//! `providers`, `comfy`, `mcp` and `serve` arrive with their milestones (docs/plan.md §4.8).

use std::process::ExitCode;

const USAGE: &str = "\
usage: nodecraft-cli <command>

commands:
  --version, -V   print the version, commit and build date
  --help, -h      print this help
";

fn main() -> ExitCode {
    let args = match std::env::args_os().skip(1).map(|arg| arg.into_string()).collect::<Result<Vec<_>, _>>() {
        Ok(args) => args,
        Err(_) => {
            eprintln!("error: command-line arguments must be valid UTF-8");
            return ExitCode::FAILURE;
        }
    };
    match args.first().map(String::as_str) {
        Some("--version" | "-V" | "version") => {
            println!("nodecraft-cli {}", nodecraft_engine::build_info::long_version());
            ExitCode::SUCCESS
        }
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
