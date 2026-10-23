use super::*;

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let (inside, outside) = (base.path().join("work"), base.path().join("elsewhere"));
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    (base, inside, outside)
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn a_folder_that_does_not_exist_is_an_error_not_an_empty_allowance() {
    let (_base, inside, _) = dirs();
    let error = FileRoots::new(&[], &[inside.join("missing")]).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    let file = inside.join("a.ncraft");
    std::fs::write(&file, b"x").unwrap();
    assert!(FileRoots::new(&[file], &[]).unwrap_err().message.contains("isn't a folder"));
    let many: Vec<PathBuf> = (0..=MAX_ROOTS).map(|_| inside.clone()).collect();
    assert!(FileRoots::new(&many, &[]).is_err());
    assert!(FileRoots::default().is_empty());
}

#[test]
fn saving_resolves_the_folder_and_stays_inside_a_write_root() {
    let (_base, inside, outside) = dirs();
    let roots = FileRoots::new(&[], std::slice::from_ref(&inside)).unwrap();
    let resolved = std::fs::canonicalize(&inside).unwrap().join("a.ncraft");
    assert_eq!(roots.save_path(s(&inside.join("a.ncraft"))).unwrap(), resolved);
    // `..` is resolved before the check, not compared as text.
    assert_eq!(roots.save_path(s(&inside.join("../elsewhere/a.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden);
    assert_eq!(roots.save_path(s(&outside.join("a.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden);
    assert_eq!(roots.save_path(s(&inside.join("sub/a.ncraft"))).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(roots.save_path(s(&inside)).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(roots.save_path(s(&inside.join(".."))).unwrap_err().kind, ErrorKind::InvalidInput);
    for relative in ["a.ncraft", "./a.ncraft", ""] {
        assert!(roots.save_path(relative).unwrap_err().message.contains("absolute"), "{relative}");
    }
}

#[test]
fn opening_needs_an_existing_file_in_a_read_or_write_root() {
    let (_base, inside, outside) = dirs();
    for dir in [&inside, &outside] {
        std::fs::write(dir.join("a.ncraft"), b"x").unwrap();
    }
    let read_only = FileRoots::new(std::slice::from_ref(&inside), &[]).unwrap();
    assert_eq!(read_only.open_path(s(&inside.join("a.ncraft"))).unwrap(), std::fs::canonicalize(inside.join("a.ncraft")).unwrap());
    assert_eq!(read_only.save_path(s(&inside.join("b.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden, "read roots aren't written");
    assert_eq!(read_only.open_path(s(&outside.join("a.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden);
    assert_eq!(read_only.open_path(s(&inside.join("missing.ncraft"))).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(read_only.open_path(s(&inside)).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn a_link_inside_a_root_cannot_lead_outside_it() {
    let (_base, inside, outside) = dirs();
    std::fs::write(outside.join("theirs.ncraft"), b"x").unwrap();
    std::os::unix::fs::symlink(outside.join("theirs.ncraft"), inside.join("link.ncraft")).unwrap();
    std::os::unix::fs::symlink(&outside, inside.join("door")).unwrap();
    let roots = FileRoots::new(&[], std::slice::from_ref(&inside)).unwrap();
    assert_eq!(roots.open_path(s(&inside.join("link.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden);
    assert_eq!(roots.save_path(s(&inside.join("link.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden);
    assert_eq!(roots.save_path(s(&inside.join("door/new.ncraft"))).unwrap_err().kind, ErrorKind::Forbidden);
    // A dangling link would be followed by the write: it is refused like any other link out.
    std::os::unix::fs::symlink(outside.join("later.ncraft"), inside.join("dangling.ncraft")).unwrap();
    assert!(roots.save_path(s(&inside.join("dangling.ncraft"))).is_err());
}
