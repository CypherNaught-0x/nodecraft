#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    let mut hash = blake3::Hasher::new();
    for name in ["lib.rs", "wire.rs", "browser.rs", "worker.rs"] {
        let file = PathBuf::from("src").join(name);
        println!("cargo:rerun-if-changed={}", file.display());
        if let Ok(bytes) = fs::read(file) {
            hash.update(&bytes);
        }
    }
    println!("cargo:rerun-if-env-changed=NODECRAFT_BUILD_SHA");
    if let Ok(sha) = env::var("NODECRAFT_BUILD_SHA") {
        hash.update(sha.as_bytes());
    } else if let Ok(output) = Command::new("git").args(["rev-parse", "HEAD"]).output()
        && output.status.success()
    {
        hash.update(&output.stdout);
    }
    let branch =
        Command::new("git").args(["symbolic-ref", "-q", "HEAD"]).output().ok().filter(|o| o.status.success()).and_then(|o| String::from_utf8(o.stdout).ok());
    for reference in [Some("HEAD"), branch.as_deref().map(str::trim)].into_iter().flatten() {
        if let Ok(output) = Command::new("git").args(["rev-parse", "--git-path", reference]).output()
            && output.status.success()
            && let Ok(path) = String::from_utf8(output.stdout)
        {
            println!("cargo:rerun-if-changed={}", path.trim());
        }
    }
    println!("cargo:rustc-env=NODECRAFT_MESH_BUILD={}", hash.finalize().to_hex());
}
