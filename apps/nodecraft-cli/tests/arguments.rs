#![cfg(any(unix, windows))]

use std::ffi::OsString;
use std::process::Command;

#[cfg(unix)]
fn invalid_argument() -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(vec![0xff])
}

#[cfg(windows)]
fn invalid_argument() -> OsString {
    use std::os::windows::ffi::OsStringExt;
    OsString::from_wide(&[0xd800])
}

#[test]
fn invalid_arguments_return_an_error_without_panicking() {
    for args in [vec![invalid_argument(), OsString::from("--version")], vec![OsString::from("--version"), invalid_argument()]] {
        let output = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("UTF-8"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}
