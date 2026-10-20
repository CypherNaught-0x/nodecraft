//! NodeCraft eye candy: paints the showcase scenes offscreen and writes the README artwork.
//!
//! This is an opt-in developer tool. It is deliberately outside the workspace (`crates/*`,
//! `apps/*` and `xtask` are the members), so `cargo xtask ci` never builds it, nothing in the
//! product can invoke it, and no CI job runs it. The committed PNGs are its output; the
//! generation command is recorded in `tools/eyecandy/README.md` and in each file's commit.
//!
//! Run it from anywhere:
//!
//! ```sh
//! cargo run --manifest-path tools/eyecandy/Cargo.toml --release -- --shots all
//! ```
#![forbid(unsafe_code)]

mod art;
mod scene;
mod shots;

use std::path::PathBuf;

use scene::Fail;

const USAGE: &str = "usage: nodecraft-eyecandy [--shots all|name,name] [--out DIR] [--scale N] [--max-width N] [--list]\n\
  \n\
  Renders the README gallery with the real egui shell, offscreen, and writes PNGs into\n\
  assets/readme by default. Nothing here runs during a build, a test or a release.";

fn main() {
    match run() {
        Ok(()) => {}
        Err(error) => {
            eprintln!("eyecandy: {error}");
            std::process::exit(1);
        }
    }
}

fn value_of(arguments: &[String], index: usize, flag: &str) -> Result<String, Fail> {
    arguments.get(index + 1).cloned().ok_or_else(|| scene::fail(format!("{flag} needs a value")))
}

/// The repository's `assets/readme`, found by walking up from the current directory.
///
/// The compile-time `CARGO_MANIFEST_DIR` would be baked into the binary, so a stale copy in a
/// shared `CARGO_TARGET_DIR` would happily write committed README images into a *different*
/// checkout while the operator was looking at this one.
fn default_out() -> Result<PathBuf, Fail> {
    let start = std::env::current_dir().map_err(|error| scene::fail(format!("cannot read the working directory: {error}")))?;
    for dir in start.ancestors() {
        if dir.join("tools").join("eyecandy").join("Cargo.toml").is_file() && dir.join("assets").join("readme").is_dir() {
            return Ok(dir.join("assets").join("readme"));
        }
    }
    Err(scene::fail("no assets/readme found above the working directory; pass --out DIR"))
}

fn run() -> Result<(), Fail> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut out = default_out()?;
    let mut wanted = String::from("all");
    let mut scale = 2.0f32;
    let mut max_width = 2_200u32;

    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments.get(index).map(String::as_str).unwrap_or("");
        let value = || value_of(&arguments, index, flag);
        match flag {
            "--out" => {
                out = PathBuf::from(value()?);
                index += 2;
            }
            "--shots" => {
                wanted = value()?;
                index += 2;
            }
            "--scale" => {
                scale = value()?.parse::<f32>().map_err(|error| scene::fail(format!("--scale: {error}")))?;
                if !scale.is_finite() || !(0.25..=4.0).contains(&scale) {
                    return Err(scene::fail("--scale must be a finite number between 0.25 and 4"));
                }
                index += 2;
            }
            "--max-width" => {
                max_width = value()?.parse::<u32>().map_err(|error| scene::fail(format!("--max-width: {error}")))?;
                if max_width < 320 {
                    return Err(scene::fail("--max-width must be at least 320: smaller writes a useless image over the target"));
                }
                index += 2;
            }
            "--list" => {
                for shot in shots::shots() {
                    println!("{:<15} {}", shot.name, shot.caption);
                }
                return Ok(());
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err(scene::fail(format!("unknown argument {other}\n{USAGE}"))),
        }
    }

    let names: Vec<String> = shots::shots().iter().map(|shot| shot.name.to_owned()).collect();
    let wanted: Vec<String> = wanted.split(',').map(|name| name.trim().to_owned()).filter(|name| !name.is_empty()).collect();
    if wanted.is_empty() {
        return Err(scene::fail(format!("--shots needs a name or all; known shots: {}", names.join(", "))));
    }
    let run_all = wanted.iter().any(|name| name == "all");
    let unknown: Vec<&str> = wanted.iter().filter(|name| !run_all && !names.iter().any(|known| known == *name)).map(String::as_str).collect();
    if !unknown.is_empty() {
        return Err(scene::fail(format!("unknown shot(s): {}; known shots: {}", unknown.join(", "), names.join(", "))));
    }
    let plates = art::plates()?;
    let total: usize = plates.iter().map(|(_, bytes)| bytes.len()).sum();
    println!("painted {} procedural plates ({:.0} KiB)", plates.len(), total as f64 / 1024.0);

    let mut written = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for shot in shots::shots() {
        if !run_all && !wanted.iter().any(|name| name == shot.name) {
            continue;
        }
        match shots::render(&shot, scene::showcase(&plates)?, scale, &out, max_width) {
            Ok(line) => {
                println!("{line}");
                written += 1;
            }
            Err(error) => {
                eprintln!("{}: {error}", shot.name);
                failed.push(shot.name.to_owned());
            }
        }
    }
    if !failed.is_empty() {
        return Err(scene::fail(format!("{} shot(s) failed: {}", failed.len(), failed.join(", "))));
    }
    if written == 0 {
        return Err(scene::fail(format!("no shot matched {wanted:?}; try --list")));
    }
    println!("{written} image(s) in {}", out.display());
    Ok(())
}
