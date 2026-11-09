use std::path::Path;
use std::process::{Command, Output};

fn command(cache: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .env("NODECRAFT_SHARED_TARGET", cache)
        .env_remove("NODECRAFT_SHARED_TARGET_HELD")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .env("CARGO_ALIAS_WIPE", "clean")
        .env("CARGO_ALIAS_FMT", "clean");
    command
}

fn run(cache: &Path, args: &[&str]) -> Output {
    command(cache).args(args).output().unwrap()
}

#[test]
fn managed_commands_neither_expand_nor_dispatch_through_cargo_aliases() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache with space");

    let refused = run(&cache, &["shared", "--", "wipe"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("aliases and arbitrary extensions are refused"), "{refused:?}");
    assert!(!cache.exists());

    let fmt = run(&cache, &["shared", "--", "fmt", "--version"]);
    assert!(fmt.status.success(), "{fmt:?}");
    assert!(String::from_utf8_lossy(&fmt.stdout).contains("rustfmt"), "{fmt:?}");
    assert!(cache.join(".nodecraft-shared-target").is_file());

    let metadata = run(&cache, &["shared", "--", "metadata", "--no-deps", "--format-version", "1"]);
    assert!(metadata.status.success(), "{metadata:?}");
    let data: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
    for key in ["target_directory", "build_directory"] {
        assert_eq!(Path::new(data.get(key).unwrap().as_str().unwrap()).canonicalize().unwrap(), cache.canonicalize().unwrap());
    }

    let clean = run(&cache, &["shared", "--clean"]);
    assert!(clean.status.success(), "{clean:?}");
    assert!(!cache.exists());
    assert!(tmp.path().join("cache with space.nodecraft-shared.lock").is_file());
}

#[test]
fn template_paths_are_refused_before_reservation() {
    let tmp = tempfile::tempdir().unwrap();
    let output = run(&tmp.path().join("{workspace-root}"), &["shared", "--", "metadata"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cargo template braces"), "{output:?}");
    assert!(tmp.path().read_dir().unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_are_refused_before_reservation() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let tmp = tempfile::tempdir().unwrap();
    let output = run(&tmp.path().join(OsStr::from_bytes(b"cache-\xff")), &["shared", "--", "metadata"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("valid UTF-8"), "{output:?}");
    assert!(tmp.path().read_dir().unwrap().next().is_none());
}

#[test]
fn performance_collection_cannot_inherit_a_shared_build_lease() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask")).env("NODECRAFT_SHARED_TARGET_HELD", "inherited").args(["perf", "collect"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("perf collect needs an isolated target"), "{output:?}");
}

#[test]
fn a_nested_shared_invocation_cannot_join_the_outer_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap())
        .env("NODECRAFT_SHARED_TARGET_HELD", "inherited")
        .env("NODECRAFT_SHARED_TARGET", &cache)
        .env("CARGO_TARGET_DIR", &cache)
        .env("CARGO_BUILD_BUILD_DIR", &cache)
        .args(["shared", "--", "metadata"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("separate invocation outside the managed command"), "{output:?}");
    assert!(!cache.exists());
}

#[cfg(unix)]
#[test]
fn signaled_cargo_child_requires_recovery_but_normal_failure_does_not() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let shim = tmp.path().join("cargo");
    fs::write(
        &shim,
        r#"#!/bin/sh
if [ "$1" = "--config" ]; then shift 4; fi
if [ "$1" = "test" ]; then
    if [ -n "${CARGO_TARGET_DIR+x}${CARGO_BUILD_TARGET_DIR+x}${CARGO_BUILD_BUILD_DIR+x}" ]; then
        exit 77
    fi
    if [ "$NODECRAFT_TEST_EXIT" = "signal" ]; then
        kill -TERM "$$"
    fi
    exit 101
fi
exec "$NODECRAFT_TEST_CARGO" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let execute = |mode| {
        command(&cache)
            .env("CARGO", &shim)
            .env("NODECRAFT_TEST_CARGO", &cargo)
            .env("NODECRAFT_TEST_EXIT", mode)
            .args(["shared", "--", "test"])
            .output()
            .unwrap()
    };

    let ordinary = execute("ordinary");
    assert!(!ordinary.status.success(), "{ordinary:?}");
    assert!(String::from_utf8_lossy(&ordinary.stderr).contains("101"), "{ordinary:?}");
    assert!(!cache.join(".nodecraft-build-active").exists());

    let signaled = execute("signal");
    assert!(!signaled.status.success(), "{signaled:?}");
    assert!(String::from_utf8_lossy(&signaled.stderr).contains("shared cache needs recovery"), "{signaled:?}");
    assert!(cache.join(".nodecraft-build-active").is_file());
    let refused = run(&cache, &["shared", "--clean"]);
    assert!(!refused.status.success(), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("was interrupted"), "{refused:?}");
}
