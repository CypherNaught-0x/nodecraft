//! Staging and swapping in updates (#540) in temporary folders: no network, no real install.
use super::*;
use zip::write::SimpleFileOptions;

fn options(mode: u32) -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored).unix_permissions(mode)
}

/// A zip of `entries` (name, contents; a name ending in `/` is a folder).
fn zipped(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, contents) in entries {
        if name.ends_with('/') {
            writer.add_directory(*name, options(0o755)).unwrap();
        } else {
            let mode = if name.contains("/MacOS/") { 0o755 } else { 0o644 };
            writer.start_file(*name, options(mode)).unwrap();
            writer.write_all(contents).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}

fn bundle_zip(marker: &[u8]) -> Vec<u8> {
    zipped(&[
        ("NodeCraft.app/", b""),
        ("NodeCraft.app/Contents/Info.plist", b"<plist>io.github.cyphernaught0x.nodecraft</plist>"),
        ("NodeCraft.app/Contents/MacOS/NodeCraft", marker),
        ("__MACOSX/._NodeCraft.app", b"metadata"),
    ])
}

fn downloaded(bytes: Vec<u8>) -> Downloaded {
    Downloaded { version: "99.0.0".into(), file: "payload".into(), bytes }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn a_zipped_bundle_unpacks_with_its_modes() {
    let dir = tempfile::tempdir().unwrap();
    let into = dir.path().join("out.app");
    unzip_bundle(&bundle_zip(b"new"), &into).unwrap();
    let exe = into.join("Contents/MacOS/NodeCraft");
    assert_eq!(std::fs::read(&exe).unwrap(), b"new");
    #[cfg(unix)]
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&exe).unwrap().permissions()) & 0o777, 0o755);
    // The macOS metadata beside the bundle is left out.
    assert_eq!(names(&into), ["Contents"]);
}

#[test]
fn hostile_or_broken_bundles_are_refused() {
    let mut symlink = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    symlink.add_directory("NodeCraft.app/Contents/MacOS/", options(0o755)).unwrap();
    symlink.add_symlink("NodeCraft.app/Contents/MacOS/NodeCraft", "/bin/sh", options(0o755)).unwrap();
    let symlink = symlink.finish().unwrap().into_inner();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not a zip", b"PK nonsense".to_vec()),
        ("escapes", zipped(&[("../evil.app/Contents/Info.plist", b"x")])),
        ("two bundles", zipped(&[("A.app/Contents/Info.plist", b"x"), ("B.app/Contents/MacOS/B", b"x")])),
        ("not a bundle", zipped(&[("NodeCraft/Contents/Info.plist", b"x")])),
        ("incomplete", zipped(&[("NodeCraft.app/Contents/Info.plist", b"x")])),
        ("symlink", symlink),
        (
            "xattrs",
            zipped(&[
                ("NodeCraft.app/Contents/Info.plist", b"x"),
                ("NodeCraft.app/Contents/MacOS/._NodeCraft", b"attrs"),
                ("NodeCraft.app/Contents/MacOS/NodeCraft", b"x"),
            ]),
        ),
    ];
    for (case, bytes) in cases {
        let dir = tempfile::tempdir().unwrap();
        let error = unzip_bundle(&bytes, &dir.path().join("out.app")).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{case}");
        assert!(error.message.contains("nothing was installed"), "{case}: {}", error.message);
        // Nothing escaped the staging folder.
        assert!(!dir.path().join("evil.app").exists(), "{case}");
    }
}

#[test]
fn a_bundle_is_staged_beside_the_running_one_and_swapped_in() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("NodeCraft.app");
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    std::fs::write(bundle.join("Contents/MacOS/NodeCraft"), b"old").unwrap();
    let kind = Kind::MacBundle { bundle: bundle.clone(), verify: false };
    let updates = dir.path().join("settings/updates");

    // A broken payload leaves nothing staged.
    assert!(stage(&kind, &downloaded(b"garbage".to_vec()), &updates).is_err());
    assert_eq!(names(dir.path()), ["NodeCraft.app"]);

    let staged = stage(&kind, &downloaded(bundle_zip(b"new")), &updates).unwrap();
    assert_eq!(staged.path, dir.path().join(".NodeCraft-update-99.0.0.app"));
    // Staging changes nothing about the running copy.
    assert_eq!(std::fs::read(bundle.join("Contents/MacOS/NodeCraft")).unwrap(), b"old");

    assert_eq!(apply(&staged, &updates).unwrap(), Relaunch::Bundle(bundle.clone()));
    assert_eq!(std::fs::read(bundle.join("Contents/MacOS/NodeCraft")).unwrap(), b"new");
    let old = format!("{BUNDLE_OLD_PREFIX}{VERSION}.app");
    assert_eq!(names(dir.path()), [old.as_str(), "NodeCraft.app"]);

    // The next start removes the old copy and says what happened.
    write_last(&updates, &LastUpdate { from: "0.0.1".into(), to: VERSION.into(), error: None });
    assert_eq!(after_start(&updates, Some(&kind)), Some(format!("NodeCraft is updated to {VERSION}.")));
    assert_eq!(names(dir.path()), ["NodeCraft.app", "settings"]);
    assert_eq!(after_start(&updates, Some(&kind)), None, "said once");
}

#[test]
fn a_failed_swap_puts_the_running_bundle_back() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("NodeCraft.app");
    std::fs::create_dir_all(&bundle).unwrap();
    let kind = Kind::MacBundle { bundle: bundle.clone(), verify: false };
    // The staged copy went missing before the restart.
    let staged = Staged { version: "99.0.0".into(), kind, path: dir.path().join(".NodeCraft-update-99.0.0.app") };
    assert!(apply(&staged, dir.path()).is_err());
    assert!(bundle.is_dir());
    assert_eq!(names(dir.path()), ["NodeCraft.app"]);
}

#[test]
fn an_appimage_is_written_beside_and_renamed_over_the_running_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("NodeCraft.AppImage");
    std::fs::write(&file, b"\x7fELF old").unwrap();
    let kind = Kind::AppImage { file: file.clone() };
    let refused = stage(&kind, &downloaded(b"#!/bin/sh".to_vec()), dir.path()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);

    let staged = stage(&kind, &downloaded(b"\x7fELF new".to_vec()), dir.path()).unwrap();
    assert_eq!(staged.path, dir.path().join(".NodeCraft.AppImage.update-99.0.0"));
    #[cfg(unix)]
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&staged.path).unwrap().permissions()) & 0o777, 0o755);
    assert_eq!(apply(&staged, dir.path()).unwrap(), Relaunch::Exec(file.clone()));
    assert_eq!(std::fs::read(&file).unwrap(), b"\x7fELF new");
    assert_eq!(names(dir.path()), ["NodeCraft.AppImage"]);

    // A staged copy from an interrupted install is removed at the next start; others aren't.
    std::fs::write(dir.path().join(".NodeCraft.AppImage.update-98.0.0"), b"half").unwrap();
    std::fs::write(dir.path().join("notes.txt"), b"mine").unwrap();
    assert_eq!(after_start(dir.path(), Some(&kind)), None);
    assert_eq!(names(dir.path()), ["NodeCraft.AppImage", "notes.txt"]);
}

#[test]
fn an_msi_is_kept_in_the_updates_folder_and_run_by_a_helper_copy() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("NodeCraft.exe");
    std::fs::write(&exe, b"MZ app").unwrap();
    let kind = Kind::WindowsMsi { exe: exe.clone() };
    let updates = dir.path().join("updates");
    assert_eq!(stage(&kind, &downloaded(b"MZ not an msi".to_vec()), &updates).unwrap_err().kind, ErrorKind::InvalidInput);

    let msi = [&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1][..], b"rest"].concat();
    let staged = stage(&kind, &downloaded(msi), &updates).unwrap();
    assert_eq!(staged.path, updates.join("nodecraft-99.0.0.msi"));
    let helper = updates.join(HELPER);
    assert_eq!(apply(&staged, &updates).unwrap(), Relaunch::Helper { helper: helper.clone(), msi: staged.path.clone(), exe: exe.clone() });
    assert_eq!(std::fs::read(&helper).unwrap(), b"MZ app");

    // The installer said no: the next start (still this version) says so and cleans up.
    write_last(&updates, &LastUpdate { from: VERSION.into(), to: "99.0.0".into(), error: Some("the installer was cancelled".into()) });
    assert_eq!(after_start(&updates, Some(&kind)), Some("The update to NodeCraft 99.0.0 didn't install: the installer was cancelled".into()));
    assert!(names(&updates).is_empty());
}

#[test]
fn records_that_arent_about_this_version_say_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_last(dir.path(), &LastUpdate { from: "1.0.0".into(), to: "2.0.0".into(), error: None });
    assert_eq!(after_start(dir.path(), None), None);
    std::fs::write(dir.path().join(LAST_UPDATE), vec![b'{'; 100_000]).unwrap();
    assert_eq!(after_start(dir.path(), None), None);
    assert!(!dir.path().join(LAST_UPDATE).exists());
    assert_eq!(after_start(dir.path(), None), None);
}

#[test]
fn only_the_appimage_this_copy_runs_from_is_updated() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nodecraft.AppImage");
    std::fs::write(&file, b"\x7fELF").unwrap();
    let mount = dir.path().join("mount");
    let exe = mount.join("usr/bin/nodecraft");
    assert_eq!(appimage(Some(file.clone()), Some(mount.clone()), &exe), Ok(Kind::AppImage { file: file.clone() }));
    // A terminal that is itself an AppImage passes APPIMAGE and APPDIR on to a tar.gz copy.
    let other = dir.path().join("other-mount");
    assert!(appimage(Some(file.clone()), Some(other), &dir.path().join("opt/nodecraft")).is_err());
    assert!(appimage(Some(file.clone()), None, &exe).is_err());
    assert!(appimage(None, Some(mount.clone()), &exe).is_err());
    assert!(appimage(Some(PathBuf::from("relative.AppImage")), Some(mount.clone()), &exe).is_err());
    assert!(appimage(Some(dir.path().join("missing.AppImage")), Some(mount), &exe).is_err());
}

#[test]
fn the_msi_install_folder_is_read_from_reg_s_answer() {
    let answer = "\r\nHKEY_LOCAL_MACHINE\\Software\\NodeCraft\r\n    InstallDir    REG_SZ    C:\\Program Files\\NodeCraft\\\r\n\r\n";
    assert_eq!(reg_value(answer).as_deref(), Some("C:\\Program Files\\NodeCraft\\"));
    assert_eq!(reg_value("ERROR: The system was unable to find the specified registry key"), None);
    assert_eq!(reg_value("    InstallDir    REG_SZ    "), None);
    assert!(same_dir(Path::new("C:\\Program Files\\NodeCraft\\"), Path::new("c:\\program files\\nodecraft")));
}

#[test]
fn waiting_for_a_hostile_pid_returns_at_once() {
    let started = std::time::Instant::now();
    for pid in ["", "-1", "1; rm -rf /", "abc"] {
        wait_for_exit(pid);
    }
    assert!(started.elapsed() < Duration::from_secs(1));
}
