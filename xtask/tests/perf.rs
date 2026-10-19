use std::{path::Path, process::Command};
fn invoke(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xtask")).current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()).args(args).output().unwrap()
}
#[test]
fn perf_cli_rejects_invalid_admission_before_building_or_creating_outputs() {
    for args in [
        vec!["perf", "collect", "--samples", "257"],
        vec!["perf", "collect", "--warmups", "0"],
        vec!["perf", "collect", "--samples", "3", "--samples", "4"],
        vec!["perf", "collect", "--machine", "private/path"],
        vec!["perf", "collect", "--unknown", "1"],
        vec!["perf", "compare"],
    ] {
        let o = invoke(&args);
        assert_eq!(o.status.code(), Some(3));
        assert!(!String::from_utf8_lossy(&o.stderr).contains("Compiling"));
    }
    let help = invoke(&["perf", "help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("no regression") || String::from_utf8_lossy(&help.stdout).contains("No GPU/FPS"));
}
