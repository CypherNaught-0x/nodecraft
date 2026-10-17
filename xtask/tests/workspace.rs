use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;
fn workspace(version: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("xtask/src/nested")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), format!("[workspace]\nmembers=[\"xtask\"]\nresolver=\"3\"\n[workspace.package]\nversion=\"{version}\"\n"))
        .unwrap();
    std::fs::write(dir.path().join("xtask/Cargo.toml"), "[package]\nname=\"xtask\"\nversion.workspace=true\nedition=\"2024\"\n").unwrap();
    std::fs::write(dir.path().join("xtask/src/lib.rs"), "// Original minimal workspace fixture.\n").unwrap();
    std::fs::write(dir.path().join("Cargo.lock"), format!("version = 4\n[[package]]\nname = \"xtask\"\nversion = \"{version}\"\n")).unwrap();
    dir
}
fn invoke(cwd: &Path, stale: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask")).current_dir(cwd).env("CARGO_MANIFEST_DIR", stale.join("xtask")).args(args).output().unwrap()
}
#[test]
fn reused_binary_selects_each_invoking_workspace_including_nested_paths() {
    let first = workspace("1.2.3");
    let second = workspace("4.5.6");
    for (cwd, expected) in [(first.path().to_path_buf(), "1.2.3"), (second.path().to_path_buf(), "4.5.6"), (second.path().join("xtask/src/nested"), "4.5.6")] {
        let output = invoke(&cwd, first.path(), &["version"]);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), expected);
    }
}

#[test]
fn version_update_and_lock_refresh_use_only_the_invoking_workspace() {
    let first = workspace("1.2.3");
    let second = workspace("4.5.6");
    let first_manifest = std::fs::read(first.path().join("Cargo.toml")).unwrap();
    let first_lock = std::fs::read(first.path().join("Cargo.lock")).unwrap();
    let output = invoke(&second.path().join("xtask/src/nested"), first.path(), &["version", "set", "7.8.9"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(std::fs::read(first.path().join("Cargo.toml")).unwrap(), first_manifest);
    assert_eq!(std::fs::read(first.path().join("Cargo.lock")).unwrap(), first_lock);
    assert!(std::fs::read_to_string(second.path().join("Cargo.toml")).unwrap().contains("7.8.9"));
    assert!(std::fs::read_to_string(second.path().join("Cargo.lock")).unwrap().contains("7.8.9"));
    let expected = second.path().canonicalize().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("xtask workspace: {}", expected.display())));
}
#[test]
fn outside_invalid_and_unrelated_workspaces_never_fall_back_to_stale_context() {
    let stale = workspace("1.2.3");
    let manifest = std::fs::read(stale.path().join("Cargo.toml")).unwrap();
    let lock = std::fs::read(stale.path().join("Cargo.lock")).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let invalid = workspace("4.5.6");
    std::fs::write(invalid.path().join("Cargo.toml"), "[workspace\nmalformed").unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    std::fs::create_dir(unrelated.path().join("src")).unwrap();
    std::fs::write(unrelated.path().join("Cargo.toml"), "[package]\nname=\"unrelated\"\nversion=\"0.1.0\"\nedition=\"2024\"\n").unwrap();
    std::fs::write(unrelated.path().join("src/lib.rs"), "").unwrap();
    std::fs::write(unrelated.path().join("Cargo.lock"), "version=4\n[[package]]\nname=\"unrelated\"\nversion=\"0.1.0\"\n").unwrap();
    for cwd in [outside.path(), invalid.path(), unrelated.path()] {
        let output = invoke(cwd, stale.path(), &["version", "set", "9.9.9"]);
        assert!(!output.status.success(), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
        assert_eq!(std::fs::read(stale.path().join("Cargo.toml")).unwrap(), manifest);
        assert_eq!(std::fs::read(stale.path().join("Cargo.lock")).unwrap(), lock);
        assert!(!stale.path().join("target").exists());
    }
}
#[test]
fn read_only_workspace_validation_does_not_create_a_missing_lockfile() {
    let dir = workspace("4.5.6");
    let lock = dir.path().join("Cargo.lock");
    std::fs::remove_file(&lock).unwrap();
    let output = invoke(dir.path(), dir.path(), &["version"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "4.5.6");
    assert!(!lock.exists());
    assert!(std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap().contains("4.5.6"));
}
