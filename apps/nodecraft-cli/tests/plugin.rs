//! `plugin pack|inventory|lint` end to end (#175), on the real binary and the example packages.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn cli(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_nodecraft-cli"))
        .args(args)
        .env("NODECRAFT_CONFIG_DIR", "")
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    assert!(output.status.success(), "failed: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> String {
    assert!(!output.status.success(), "should fail: {}", String::from_utf8_lossy(&output.stdout));
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins").join(name)
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// Packs `folder` into `out`; the pack report.
fn pack(folder: &Path, out: &Path) -> Value {
    json(&cli(&["plugin", "pack", s(folder), "--out", s(out), "--json"]))
}

#[test]
fn the_examples_pack_to_frozen_digests_and_pass_lint() {
    let out = tempfile::tempdir().unwrap();
    for (name, digest, runtime) in [("palette", PALETTE, "data"), ("shout", SHOUT, "text_actions")] {
        let file = out.path().join(format!("{name}.ncraft-plugin"));
        let report = pack(&example(name), &file);
        assert_eq!(report["digest"], digest, "{name}: the same files give the same digest everywhere");
        assert_eq!(json(&cli(&["plugin", "inventory", s(&example(name)), "--json"]))["digest"], digest, "inventory agrees with pack");
        let lint = json(&cli(&["plugin", "lint", s(&file), "--json"]));
        assert_eq!(lint["digest"], digest);
        assert_eq!(lint["runtime"], runtime);
        assert_eq!(lint["provenance"], "unsigned");
        assert_eq!(lint["registrations"], serde_json::json!([format!("plugin.io.example.{name}.{}", if name == "palette" { "warm" } else { "shout" })]));
        assert!(lint["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("isn't signed")));
        let text = cli(&["plugin", "lint", s(&file)]);
        assert!(text.status.success());
        let text = String::from_utf8(text.stdout).unwrap();
        assert!(text.contains("passes") && text.contains("rights: none asked for"), "{text}");
    }
}

const PALETTE: &str = "blake3:8b20c7c056a67467b8d64c8040ad95c7250fc9859bbf05ea3d77dbed36d88eee";
const SHOUT: &str = "blake3:e259829c9dd10f6a58fb79815a88927694372a84be18cb665f4a9b0e402adcd8";

#[test]
fn packing_twice_gives_the_same_bytes() {
    let out = tempfile::tempdir().unwrap();
    let (a, b) = (out.path().join("a.ncraft-plugin"), out.path().join("b.ncraft-plugin"));
    pack(&example("shout"), &a);
    pack(&example("shout"), &b);
    let bytes = std::fs::read(a).unwrap();
    assert_eq!(bytes, std::fs::read(b).unwrap());
    // Not just the digest: the archive bytes are frozen too, so no clock or file mode gets in.
    assert_eq!(blake3::hash(&bytes).to_hex().as_str(), SHOUT_BYTES);
}

const SHOUT_BYTES: &str = "d02d8fd116b13afca3a5f247f598669656a45bcb3587a48c34d3844d778d269d";

/// Copies an example into a fresh folder, changed by `edit`.
fn changed(name: &str, edit: impl FnOnce(&Path)) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut pending = vec![(example(name), dir.path().to_path_buf())];
    while let Some((from, to)) = pending.pop() {
        std::fs::create_dir_all(&to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push((entry.path(), to.join(entry.file_name())));
            } else {
                std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
            }
        }
    }
    edit(dir.path());
    dir
}

type Edit = Box<dyn FnOnce(&Path)>;

#[test]
fn a_folder_that_cant_be_a_package_is_refused_before_anything_is_written() {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.ncraft-plugin");
    let cases: Vec<(&str, Edit, &str)> = vec![
        ("no manifest", Box::new(|d: &Path| std::fs::remove_file(d.join("manifest.json")).unwrap()), "manifest.json"),
        ("no license", Box::new(|d: &Path| std::fs::remove_file(d.join("LICENSE")).unwrap()), "LICENSE"),
        ("reserved name", Box::new(|d: &Path| std::fs::write(d.join("inventory.json"), "{}").unwrap()), "inventory.json"),
        ("broken manifest", Box::new(|d: &Path| std::fs::write(d.join("manifest.json"), "{oops").unwrap()), "manifest.json"),
        (
            "too many files",
            Box::new(|d: &Path| (0..1025).for_each(|i| std::fs::write(d.join(format!("data/f{i}.json")), "{}").unwrap())),
            "more than 1024 files",
        ),
        ("nested too deep", Box::new(|d: &Path| std::fs::create_dir_all(d.join("a/b/c/d/e/f/g/h")).unwrap()), "nests too deep"),
    ];
    for (what, edit, mentions) in cases {
        let dir = changed("palette", edit);
        let error = stderr(&cli(&["plugin", "pack", s(dir.path()), "--out", s(&file)]));
        assert!(error.contains(mentions), "{what}: {error}");
        assert!(!file.exists(), "{what}: nothing written");
    }
    // The output may not land inside the folder it packs.
    let dir = changed("palette", |_| {});
    let inside = dir.path().join("p.ncraft-plugin");
    assert!(stderr(&cli(&["plugin", "pack", s(dir.path()), "--out", s(&inside)])).contains("outside"));
    // Dot-files are left out: a .git folder doesn't change the package.
    let dotted = changed("palette", |d| {
        std::fs::create_dir(d.join(".git")).unwrap();
        std::fs::write(d.join(".git/HEAD"), "ref").unwrap();
        std::fs::write(d.join(".DS_Store"), "x").unwrap();
    });
    assert_eq!(pack(dotted.path(), &file)["digest"], PALETTE);
}

#[cfg(unix)]
#[test]
fn a_link_in_the_folder_is_refused_not_followed() {
    let out = tempfile::tempdir().unwrap();
    let dir = changed("palette", |d| std::os::unix::fs::symlink("/etc/hosts", d.join("data/hosts")).unwrap());
    let error = stderr(&cli(&["plugin", "pack", s(dir.path()), "--out", s(&out.path().join("p.ncraft-plugin"))]));
    assert!(error.contains("is a link"), "{error}");
}

#[test]
fn lint_refuses_what_install_grant_or_enable_would() {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.ncraft-plugin");
    // A text action that doesn't compile packs, but doesn't pass lint.
    let broken = changed("shout", |d| {
        let def = std::fs::read_to_string(d.join("actions/shout.json")).unwrap().replace("upper(words)", "upper(nothing)");
        std::fs::write(d.join("actions/shout.json"), def).unwrap();
    });
    pack(broken.path(), &file);
    let error = stderr(&cli(&["plugin", "lint", s(&file)]));
    assert!(error.contains("doesn't pass"), "{error}");
    // A host protocol this NodeCraft doesn't speak.
    let future = changed("palette", |d| {
        let m = std::fs::read_to_string(d.join("manifest.json")).unwrap().replace(r#""major": 1, "min_minor""#, r#""major": 9, "min_minor""#);
        std::fs::write(d.join("manifest.json"), m).unwrap();
    });
    pack(future.path(), &file);
    assert!(stderr(&cli(&["plugin", "lint", s(&file)])).contains("doesn't pass"));
    // Not a package at all, and a missing file.
    std::fs::write(&file, b"not a package").unwrap();
    assert!(stderr(&cli(&["plugin", "lint", s(&file)])).contains("doesn't pass"));
    assert!(stderr(&cli(&["plugin", "lint", s(&out.path().join("absent"))])).contains("doesn't pass"));
    // A trust file that is broken is reported, not treated as empty.
    pack(&example("palette"), &file);
    let trust = out.path().join("trust.json");
    std::fs::write(&trust, b"{oops").unwrap();
    assert!(stderr(&cli(&["plugin", "lint", s(&file), "--trust", s(&trust)])).contains("trust"));
    // A trust file named but missing is an error, and a relative one is read.
    assert!(stderr(&cli(&["plugin", "lint", s(&file), "--trust", s(&out.path().join("absent.json"))])).contains("doesn't exist"));
    let relative =
        Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["plugin", "lint", s(&file), "--trust", "trust.json"]).current_dir(out.path()).output().unwrap();
    assert!(String::from_utf8_lossy(&relative.stderr).contains("trust"), "the relative trust file is the one read");
}

/// Rewrites the palette example's manifest with `edit`.
fn manifest(edit: impl FnOnce(String) -> String) -> tempfile::TempDir {
    changed("palette", |d| {
        let m = std::fs::read_to_string(d.join("manifest.json")).unwrap();
        std::fs::write(d.join("manifest.json"), edit(m)).unwrap();
    })
}

#[test]
fn lint_refuses_reserved_names_and_rights_this_build_cant_grant() {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.ncraft-plugin");
    let reserved = manifest(|m| m.replace("io.example.palette", "nodecraft.example.palette"));
    pack(reserved.path(), &file);
    let error = stderr(&cli(&["plugin", "lint", s(&file)]));
    assert!(error.contains("doesn't pass") && error.contains("nodecraft"), "{error}");
    // A data package can't be granted the GPU: needing it isn't even a package, wanting it is a warning.
    let needs = manifest(|m| m.replace(r#""required": [], "optional": []"#, r#""required": ["gpu"], "optional": []"#));
    let error = stderr(&cli(&["plugin", "pack", s(needs.path()), "--out", s(&file)]));
    assert!(error.contains("capabilities"), "{error}");
    let wants = manifest(|m| m.replace(r#""required": [], "optional": []"#, r#""required": [], "optional": ["gpu"]"#));
    pack(wants.path(), &file);
    let lint = json(&cli(&["plugin", "lint", s(&file), "--json"]));
    assert!(lint["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("optional gpu")), "{lint}");
}

#[test]
fn bad_arguments_are_explained() {
    assert!(stderr(&cli(&["plugin"])).contains("pack, inventory or lint"));
    assert!(stderr(&cli(&["plugin", "pack", "."])).contains("--out"));
    assert!(stderr(&cli(&["plugin", "lint", "a", "b"])).contains("one package file"));
    assert!(stderr(&cli(&["plugin", "lint", "a", "--out", "b"])).contains("doesn't take --out"));
    assert!(stderr(&cli(&["plugin", "sign", "a"])).contains("unknown"));
}
