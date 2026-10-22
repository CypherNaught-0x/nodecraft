use super::*;
use std::path::PathBuf;

fn absolute(name: &str) -> PathBuf {
    std::env::temp_dir().join("nodecraft-tool-discovery-tests").join(name)
}

#[test]
fn environment_then_settings_then_path_per_tool_without_persisting_discovery() {
    let overrides = absolute("environment/ffmpeg");
    let configured = MediaSettings { ffmpeg: Some(absolute("settings/ffmpeg")), ffprobe: Some(absolute("settings/ffprobe")) };
    let path = std::env::join_paths([absolute("first"), absolute("second")]).unwrap();
    let got = resolve(&configured, Some(overrides.as_os_str()), None, Some(&path), |_| panic!("explicit tools never search PATH")).unwrap();
    assert_eq!(got.ffmpeg, Some(overrides));
    assert_eq!(got.ffprobe, configured.ffprobe);
    let original = MediaSettings::default();
    let mut searched = Vec::new();
    let got = resolve(&original, None, None, Some(&path), |p| {
        searched.push(p.to_path_buf());
        p.parent() == Some(absolute("second").as_path())
    })
    .unwrap();
    assert_eq!(got.ffmpeg.as_deref().and_then(|p| p.parent()), Some(absolute("second").as_path()));
    assert_eq!(got.ffprobe.as_deref().and_then(|p| p.parent()), Some(absolute("second").as_path()));
    assert_eq!(searched.len(), 4);
    assert_eq!(original, MediaSettings::default());
}

#[test]
fn invalid_explicit_choices_refuse_without_falling_back() {
    let path = std::env::join_paths([absolute("bin")]).unwrap();
    for invalid in [OsStr::new(""), OsStr::new("ffmpeg"), OsStr::new("relative/ffmpeg")] {
        assert!(resolve(&MediaSettings::default(), Some(invalid), None, Some(&path), |_| panic!("must not fall back")).is_err());
    }
    let configured = MediaSettings { ffmpeg: Some("relative/ffmpeg".into()), ffprobe: None };
    assert!(resolve(&configured, None, None, Some(&path), |_| panic!("must not fall back")).is_err());
}

#[test]
fn current_directory_entries_are_skipped_and_search_is_bounded() {
    let path = std::env::join_paths([PathBuf::new(), PathBuf::from("relative"), absolute("bin with spaces")]).unwrap();
    let mut searched = Vec::new();
    let got = resolve(&MediaSettings::default(), None, None, Some(&path), |p| {
        searched.push(p.to_path_buf());
        false
    })
    .unwrap();
    assert_eq!(got.tools(), None);
    assert_eq!(searched.len(), 2);
    assert!(searched.iter().all(|p| p.parent() == Some(absolute("bin with spaces").as_path())));
    let path = std::env::join_paths((0..=MAX_DIRECTORIES).map(|n| absolute(&n.to_string()))).unwrap();
    let mut calls = 0;
    assert!(
        resolve(&MediaSettings::default(), None, None, Some(&path), |_| {
            calls += 1;
            false
        })
        .is_err()
    );
    assert_eq!(calls, MAX_DIRECTORIES);
    let oversized = "x".repeat(MAX_PATH_BYTES + 1);
    assert!(resolve(&MediaSettings::default(), None, None, Some(OsStr::new(&oversized)), |_| panic!("oversized path")).is_err());
}

#[test]
fn executable_candidates_must_be_files_and_executable_on_unix() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!executable(dir.path()));
    let path = dir.path().join("tool");
    assert!(!executable(&path));
    std::fs::write(&path, b"test fixture; never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!executable(&path));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert!(executable(&path));
}
