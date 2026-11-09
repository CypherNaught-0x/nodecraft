//! `cargo xtask browser`: the tests that must run in a real browser (#1038), compiled to
//! WebAssembly and run headless by `wasm-bindgen-test-runner` (from `wasm-bindgen-cli`) through a
//! WebDriver (`chromedriver` on `PATH`, or `CHROMEDRIVER`). The runner must be the version of
//! wasm-bindgen in `Cargo.lock`, or it refuses the test binary: this checks that first and says
//! how to install the right one.
use std::path::{Path, PathBuf};
use std::process::Command;

/// The test targets that run in a browser, and how many tests each holds: a target whose tests
/// stopped compiling in (a `cfg` that no longer matches) would otherwise pass with none run.
const TESTS: &[(&str, &[&str], usize)] = &[("nodecraft-engine", &["--test", "browser_plugins_wasm"], 2), ("nodecraft-mesh-preview", &["--lib"], 3)];

pub fn run(root: &Path) -> Result<(), String> {
    let wanted = locked(root, "wasm-bindgen")?;
    let runner = std::env::var_os("WASM_BINDGEN_TEST_RUNNER").map_or_else(|| PathBuf::from("wasm-bindgen-test-runner"), PathBuf::from);
    let install = format!("install it with `cargo install wasm-bindgen-cli --version {wanted} --locked`");
    let output = Command::new(&runner).arg("--version").output().map_err(|_| format!("{} isn't installed; {install}", runner.display()))?;
    let version = String::from_utf8_lossy(&output.stdout);
    let found = version.split_whitespace().nth(1).unwrap_or_default();
    if found != wanted {
        return Err(format!("{} is version {found:?}, but Cargo.lock has wasm-bindgen {wanted}; {install}", runner.display()));
    }
    for (package, target, count) in TESTS {
        let mut c = crate::cargo(root);
        c.args(["test", "-p", package, "--target", "wasm32-unknown-unknown"]);
        c.args(*target);
        c.env("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER", &runner);
        let what = format!("cargo test -p {package} --target wasm32-unknown-unknown {} (in a headless browser)", target.join(" "));
        eprintln!("$ {what}");
        let output = c.output().map_err(|e| format!("{what}: failed to spawn: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        print!("{stdout}");
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        if !output.status.success() {
            return Err(format!("{what}: {}", output.status));
        }
        let passed = passed(&stdout);
        if passed != Some(*count) {
            return Err(format!("{what}: expected {count} tests to pass in the browser, the runner reported {passed:?}"));
        }
    }
    Ok(())
}

/// How many tests a `test result: ok. N passed; ...` line reports.
fn passed(output: &str) -> Option<usize> {
    output.lines().find_map(|line| line.trim().strip_prefix("test result: ok. ")?.split_once(" passed")?.0.parse().ok())
}

/// The version of `name` in `Cargo.lock`, when exactly one is locked.
fn locked(root: &Path, name: &str) -> Result<String, String> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).map_err(|e| format!("can't read Cargo.lock: {e}"))?;
    let header = format!("name = \"{name}\"");
    let versions: Vec<&str> = lock
        .split("[[package]]")
        .filter(|package| package.lines().any(|line| line.trim() == header))
        .filter_map(|package| package.lines().find_map(|line| line.trim().strip_prefix("version = \"")?.strip_suffix('"')))
        .collect();
    match versions.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(format!("Cargo.lock has no {name}")),
        _ => Err(format!("Cargo.lock has several versions of {name}: {versions:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_locked_version_is_read_from_its_package() {
        let dir = std::env::temp_dir().join(format!("xtask-browser-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.lock"),
            "[[package]]\nname = \"wasm-bindgen-shared\"\nversion = \"9.9.9\"\n\n[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.129\"\n",
        )
        .unwrap();
        assert_eq!(locked(&dir, "wasm-bindgen").unwrap(), "0.2.129");
        assert!(locked(&dir, "js-sys").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_passing_run_reports_its_count() {
        assert_eq!(passed("running 2 tests\n\ntest result: ok. 2 passed; 0 failed; 0 ignored\n"), Some(2));
        assert_eq!(passed("no tests to run!\n"), None);
        assert_eq!(passed("test result: FAILED. 1 passed; 1 failed\n"), None);
    }
}
