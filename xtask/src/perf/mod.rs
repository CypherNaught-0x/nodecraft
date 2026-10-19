//! Bounded release regression tooling (#152), separate from ordinary timing assertions.
mod collect;
mod config;
mod report;
#[cfg(test)]
mod tests;
pub fn run(root: &std::path::Path, args: &[&str]) -> Result<u8, String> {
    match args.first().copied() {
        Some("collect") => collect::collect(root, args.get(1..).unwrap_or_default()),
        Some("compare") => collect::comparison(root, args.get(1..).unwrap_or_default()),
        Some("--help" | "help") | None => {
            println!(
                "cargo xtask perf collect --machine PUBLIC_CLASS --out NEW_DIRECTORY [--samples 120] [--warmups 3]\ncargo xtask perf compare BASE.json CURRENT.json --out NEW_DIRECTORY\nExit: 0 collected/within measured envelopes; 2 regression signal; 3 incomplete/incomparable/tool failure. No GPU/FPS claim."
            );
            Ok(0)
        }
        _ => Err("unknown perf action; use collect or compare".into()),
    }
}
