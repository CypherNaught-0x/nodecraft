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
    assert!(stderr(&cli(&["plugin"])).contains("pack, inventory, lint or key"));
    assert!(stderr(&cli(&["plugin", "pack", "."])).contains("--out"));
    assert!(stderr(&cli(&["plugin", "lint", "a", "b"])).contains("one package file"));
    assert!(stderr(&cli(&["plugin", "lint", "a", "--out", "b"])).contains("doesn't take --out"));
    assert!(stderr(&cli(&["plugin", "sign", "a"])).contains("unknown"));
}

/// The secret in a key file, read the way an attacker scanning output would.
fn secret_of(key: &Path) -> String {
    let file: Value = serde_json::from_slice(&std::fs::read(key).unwrap()).unwrap();
    file["secret_key"].as_str().unwrap().to_owned()
}

#[test]
fn an_author_signs_a_package_that_lint_verifies_against_the_printed_trust_entry() {
    let out = tempfile::tempdir().unwrap();
    let key = out.path().join("author.key");
    let made = json(&cli(&["plugin", "key", "new", s(&key), "--id", "author-key", "--json"]));
    assert_eq!(made["key_id"], "author-key");
    let secret = secret_of(&key);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600, "only its owner reads a key file");
    }
    // A key file is never replaced.
    assert!(stderr(&cli(&["plugin", "key", "new", s(&key), "--id", "author-key"])).contains("never replaced"));
    assert_eq!(secret_of(&key), secret);

    let entry = json(&cli(&["plugin", "key", "trust", s(&key), "--provenance", "From the author, in person"]));
    assert_eq!((&entry["key_id"], &entry["public_key"], &entry["status"]), (&made["key_id"], &made["public_key"], &serde_json::json!("trusted")));
    let trust = out.path().join("trust.json");
    std::fs::write(&trust, serde_json::json!({"trust_version": 1, "keys": [entry]}).to_string()).unwrap();

    let file = out.path().join("palette.ncraft-plugin");
    let packed = cli(&["plugin", "pack", s(&example("palette")), "--out", s(&file), "--sign", s(&key)]);
    assert!(packed.status.success(), "{}", String::from_utf8_lossy(&packed.stderr));
    let shown = String::from_utf8(packed.stdout).unwrap();
    assert!(shown.contains("signed by author-key") && shown.contains(PALETTE), "signing doesn't change the digest: {shown}");
    let lint = json(&cli(&["plugin", "lint", s(&file), "--trust", s(&trust), "--json"]));
    assert_eq!(
        (&lint["provenance"], &lint["signed_by"], &lint["digest"]),
        (&serde_json::json!("verified"), &serde_json::json!("author-key"), &serde_json::json!(PALETTE))
    );
    // Without the trust file it is signed by an unknown key.
    let lint = json(&cli(&["plugin", "lint", s(&file), "--json"]));
    assert!(lint["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("isn't in the trust file")), "{lint}");

    // The secret appears in no command's output.
    for output in [
        cli(&["plugin", "key", "new", s(&out.path().join("second.key")), "--id", "second"]),
        cli(&["plugin", "key", "trust", s(&key)]),
        cli(&["plugin", "pack", s(&example("palette")), "--out", s(&out.path().join("again.ncraft-plugin")), "--sign", s(&key), "--json"]),
    ] {
        let all = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success() && !all.contains(&secret), "{all}");
    }
}

#[test]
fn signing_refuses_a_key_inside_the_folder_a_broken_key_and_the_wrong_publisher() {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.ncraft-plugin");
    // A key inside the folder would be packed with it, even as a dot-file a later edit might keep.
    let dir = changed("palette", |_| {});
    let inside = dir.path().join(".author.key");
    json(&cli(&["plugin", "key", "new", s(&inside), "--id", "author-key", "--json"]));
    let error = stderr(&cli(&["plugin", "pack", s(dir.path()), "--out", s(&file), "--sign", s(&inside)]));
    assert!(error.contains("outside the folder") && !file.exists(), "{error}");
    // Packing over the key file would destroy the only copy of the secret.
    let key = out.path().join("only-copy.key");
    json(&cli(&["plugin", "key", "new", s(&key), "--id", "author-key", "--json"]));
    let before = std::fs::read(&key).unwrap();
    let error = stderr(&cli(&["plugin", "pack", s(&example("palette")), "--out", s(&key), "--sign", s(&key)]));
    assert!(error.contains("names the key file") && std::fs::read(&key).unwrap() == before, "{error}");
    // A damaged key file names what is wrong, not what it holds.
    let broken = out.path().join("broken.key");
    std::fs::write(&broken, r#"{"algorithm":"ed25519","key_id":"k","key_version":1,"secret_key":"not-hex-secret-value"}"#).unwrap();
    let error = stderr(&cli(&["plugin", "pack", s(&example("palette")), "--out", s(&file), "--sign", s(&broken)]));
    assert!(error.contains("isn't a NodeCraft signing key") && error.contains("/secret_key") && !error.contains("not-hex-secret-value"), "{error}");
    // A manifest naming its publisher can only be signed by that key.
    let key = out.path().join("author.key");
    json(&cli(&["plugin", "key", "new", s(&key), "--id", "author-key", "--json"]));
    let named = manifest(|m| m.replace(r#""license": "CC0-1.0","#, r#""license": "CC0-1.0", "publisher_key_id": "someone-else","#));
    let error = stderr(&cli(&["plugin", "pack", s(named.path()), "--out", s(&file), "--sign", s(&key)]));
    assert!(error.contains("publisher_key_id is someone-else"), "{error}");
    assert!(stderr(&cli(&["plugin", "key", "new", s(&out.path().join("x.key")), "--id", "Bad Id"])).contains("key ID"));
    assert!(stderr(&cli(&["plugin", "key"])).contains("new or trust"));
    assert!(stderr(&cli(&["plugin", "key", "new", s(&out.path().join("y.key"))])).contains("--id"));
}

/// Packs `folder` and lints the result; the error an author sees from whichever step refuses it.
fn refusal(folder: &Path) -> String {
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("p.ncraft-plugin");
    let packed = cli(&["plugin", "pack", s(folder), "--out", s(&file)]);
    if !packed.status.success() {
        return String::from_utf8(packed.stderr).unwrap();
    }
    stderr(&cli(&["plugin", "lint", s(&file)]))
}

/// The lint corpus (#866): each `fixtures/lint/invalid/<code>--<what>/` folder is a package an
/// author might write that packing or linting refuses, naming `<code>` (or, for a check with no
/// wire code, its words).
#[test]
fn every_package_in_the_lint_corpus_is_refused_with_its_code() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/plugin/fixtures/lint/invalid");
    let mut seen = 0;
    for entry in std::fs::read_dir(&corpus).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let (code, _) = name.split_once("--").unwrap_or_else(|| panic!("{name} isn't <code>--<what>"));
        // Only what the refusal says counts, not the folder's name it may print.
        let error = refusal(&path).replace(path.to_str().unwrap(), "<folder>").replace(&name, "<folder>");
        assert!(error.contains(code) || error.contains(&code.replace('_', " ")), "{name}: {error}");
        seen += 1;
    }
    assert!(seen >= 6, "the corpus has only {seen} cases");
}

/// Packages over a budget are too big to keep as files, so they are written here.
#[test]
fn a_package_over_a_budget_is_refused_with_too_large() {
    let registrations = manifest(|m| {
        let one = r#"{ "id": "warm", "kind": "catalog", "binding": "color.palette", "descriptor": "data/warm.json" }"#;
        let many: Vec<String> = (0..=256).map(|i| one.replace(r#""warm","#, &format!(r#""warm{i}","#))).collect();
        m.replace(one, &many.join(",\n    "))
    });
    let error = refusal(registrations.path());
    assert!(error.contains("too_large") && error.contains("/registrations"), "{error}");
    let descriptor = changed("palette", |d| {
        let colors: Vec<String> = (0..9000).map(|i| format!("\"#{i:06x}\"")).collect();
        std::fs::write(d.join("data/warm.json"), format!(r#"{{"name":"Warm","colors":[{}]}}"#, colors.join(","))).unwrap();
    });
    let error = refusal(descriptor.path());
    assert!(error.contains("too_large"), "{error}");
}
