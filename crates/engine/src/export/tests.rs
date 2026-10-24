use super::*;

#[test]
fn only_an_absolute_file_path_is_taken() {
    // `/tmp/out.png` has no drive on Windows, so the absolute path comes from the platform.
    let root = std::env::temp_dir();
    assert!(check(&root.join("out.png")).is_ok());
    for bad in ["", "out.png", "../out.png", "/"] {
        let error = check(Path::new(bad)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{bad:?}");
        assert!(bad.is_empty() || !error.message.contains(bad), "{}", error.message);
    }
    assert!(check(&root.join("a".repeat(MAX_PATH_BYTES))).is_err());
    assert!(check(&root.join("a\0b")).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn write_failures_say_what_to_do() {
    use std::io::ErrorKind as Io;
    assert_eq!(io_error(Io::AlreadyExists).kind, ErrorKind::Conflict);
    assert!(io_error(Io::StorageFull).message.contains("disk is full"));
    assert!(io_error(Io::QuotaExceeded).message.contains("disk is full"));
    assert!(io_error(Io::PermissionDenied).message.contains("another folder"));
    assert!(io_error(Io::Interrupted).message.contains("couldn't be written"));
}
