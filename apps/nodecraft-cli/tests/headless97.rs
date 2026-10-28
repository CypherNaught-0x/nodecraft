//! Actual CLI binary, isolated settings and local engine commands; no provider network calls.
use nodecraft_engine::{
    Session,
    library::{FileLibrary, LibraryStore},
};
use nodecraft_format::{AssetStore, Snapshot};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};
const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
fn cli(settings: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(args).env("NODECRAFT_CONFIG_DIR", settings).output().unwrap()
}
fn report(output: Output) -> Value {
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}
fn source() -> Session {
    let mut session = nodecraft_automation::blank_session(ProjectId::from_bytes([97; 16]), C).unwrap();
    session.dispatch("node.create", &json!({"canvas":C,"id":N,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    session.dispatch("node.title", &json!({"canvas":C,"id":N,"title":"Brief"})).unwrap();
    session
}
fn save(session: &Session, path: &Path) -> Vec<u8> {
    let bytes = nodecraft_format::encode(&Snapshot { project: session.document().clone(), assets: AssetStore::default(), epoch: 0 }).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}
#[test]
fn cli97_binary_run_quote_and_csv_batch_share_named_values_without_changing_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let path = dir.path().join("project.ncraft");
    let original = save(&source(), &path);
    let project = path.to_str().unwrap();
    let out = dir.path().to_str().unwrap();
    let quoted = report(cli(&settings, &["quote", project, "--input", "Brief=quoted only", "--json"]));
    assert_eq!(quoted["allowed"], true);
    assert_eq!(quoted["complete"], true);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "quote creates no files");
    let ran = report(cli(&settings, &["run", project, "--input", "Brief=a=b, 雪", "--out", out, "--json"]));
    assert_eq!(ran["results"][0]["text"], "a=b, 雪");
    let csv = dir.path().join("rows.csv");
    std::fs::write(&csv, "Brief\r\n\"first, row\"\r\n\"second\nline\"\r\n").unwrap();
    let csv_arg = csv.to_str().unwrap();
    let quoted = report(cli(&settings, &["batch", project, "--csv", csv_arg, "--quote", "--json"]));
    assert_eq!(quoted["quoted"], true);
    assert_eq!(quoted["rows"].as_array().unwrap().len(), 2);
    let batch = report(cli(&settings, &["batch", project, "--csv", csv_arg, "--out", out, "--json"]));
    assert_eq!(batch["rows"][0]["results"][0]["text"], "first, row");
    assert_eq!(batch["rows"][1]["results"][0]["text"], "second\nline");
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let folders = || std::fs::read_dir(dir.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("batch-")).count();
    let before = folders();
    std::fs::write(&csv, "Brief\nvalid\ninvalid,extra\n").unwrap();
    let invalid = cli(&settings, &["batch", project, "--csv", csv_arg, "--out", out, "--json"]);
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("row 2"));
    assert_eq!(folders(), before, "malformed later rows cannot create/run an earlier row");
}
#[test]
fn cli97_binary_runs_a_named_library_technique_and_refuses_unknown_versions() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let mut session = source();
    let version = CanvasId::from_bytes([3; 16]);
    let action = NodeId::from_bytes([6; 16]);
    session.dispatch("node.create", &json!({"canvas":C,"id":action,"kind":"text.replace","pos":{"x":300,"y":0}})).unwrap();
    for name in ["find", "replacement"] {
        session.dispatch("node.param_set", &json!({"canvas":C,"id":action,"name":name,"value":nodecraft_types::Value::Text("cat".into())})).unwrap();
    }
    session.dispatch("edge.connect",&json!({"canvas":C,"edge":{"id":nodecraft_types::EdgeId::from_bytes([7;16]),"from":{"node":N,"port":"out"},"to":{"node":action,"port":"text"},"order":0}})).unwrap();
    session.dispatch("technique.publish",&json!({"canvas":C,"published":version,"seed":NodeId::from_bytes([4;16]),"technique":nodecraft_types::TechniqueId::from_bytes([5;16]),"name":"Echo","inputs":[{"node":N,"name":"Brief"}],"outputs":[{"node":action,"name":"Result"}],"params":[]})).unwrap();
    let entry = nodecraft_engine::library::entry(&session.document().canvases()[&version]).unwrap();
    let bytes = serde_json::to_vec(&json!({"version":1,"techniques":[entry]})).unwrap();
    nodecraft_engine::library::Library::from_bytes(&bytes).unwrap();
    FileLibrary::new(settings.join("library")).save(&bytes).unwrap();
    let result =
        report(cli(&settings, &["run", "technique:Echo@1", "--input", r#"Brief=json:{"answer":42}"#, "--out", dir.path().to_str().unwrap(), "--json"]));
    assert!(result["results"].as_array().unwrap().iter().any(|r| r["text"] == r#"json:{"answer":42}"#), "{result}");
    assert_eq!(FileLibrary::new(settings.join("library")).load().unwrap().unwrap(), bytes);
    for name in ["technique:Echo@0", "technique:Echo@2", "technique:Missing"] {
        let refused = cli(&settings, &["quote", name, "--json"]);
        assert!(!refused.status.success());
        assert!(!String::from_utf8_lossy(&refused.stderr).contains("panicked"));
    }
}
#[test]
fn cli97_binary_lists_comfy_without_a_server_and_rejects_invalid_commands_without_requests() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(report(cli(dir.path(), &["comfy", "servers", "--json"])), json!([]));
    for args in [
        vec!["providers", "test", "not-a-provider", "--json"],
        vec!["comfy", "servers", "missing", "--test"],
        vec!["comfy", "import", "flow.json"],
        vec!["batch", "--csv", "missing.csv"],
    ] {
        let output = cli(dir.path(), &args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
    }
    let help = cli(dir.path(), &["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    for name in ["--input", "--technique", "--keep-going", "--max-total", "providers test", "comfy import"] {
        assert!(text.contains(name), "{name}");
    }
}

#[test]
fn cli97_binary_quotes_embedded_media_read_only_and_runs_it_through_streaming_storage() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let mut session = source();
    let node = NodeId::from_bytes([8; 16]);
    let wav = include_bytes!("../../../crates/providers/src/fal/fixtures/tone.wav");
    let asset = nodecraft_engine::import::import_media(&mut session, &nodecraft_engine::import::Incoming { name: "tone.wav", bytes: wav }).unwrap();
    session.dispatch("node.create", &json!({"canvas":C,"id":node,"kind":"source.audio","pos":{"x":300,"y":0}})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas":C,"id":node,"name":"asset","value":nodecraft_types::Value::Media(asset)})).unwrap();
    let path = dir.path().join("media.ncraft");
    let bytes = nodecraft_format::encode(&Snapshot { project: session.document().clone(), assets: session.assets().clone(), epoch: 0 }).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let project = path.to_str().unwrap();
    let quoted = report(cli(&settings, &["quote", project, "--input", "Brief=with media", "--json"]));
    assert_eq!(quoted["allowed"], true, "{quoted}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "pure quotes must not write native storage");
    let ran = report(cli(&settings, &["run", project, "--out", dir.path().to_str().unwrap(), "--json"]));
    let media = ran["results"].as_array().unwrap().iter().find(|r| r["path"].is_string()).unwrap();
    assert_eq!(std::fs::read(media["path"].as_str().unwrap()).unwrap(), wav);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn cli97_binary_server_listing_redacts_url_secrets_in_both_output_modes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        r#"[providers]
[[providers.comfyui]]
name = "GPU"
url = "http://fixture-user:fixture-password@127.0.0.1:8188/comfy?token=fixture-query#fixture-fragment"
[[providers.comfyui]]
name = "Invalid"
url = "invalid fixture-secret"
"#,
    )
    .unwrap();
    for args in [vec!["comfy", "servers", "--json"], vec!["comfy", "servers"]] {
        let output = cli(dir.path(), &args);
        assert!(output.status.success(), "{output:?}");
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        for hidden in ["fixture-user", "fixture-password", "fixture-query", "fixture-fragment", "fixture-secret"] {
            assert!(!text.contains(hidden), "{hidden}: {text}");
        }
        assert!(text.contains("http://127.0.0.1:8188/comfy"), "{text}");
    }
}
#[test]
fn cli97_binary_source_text_preserves_literal_json_prefixes() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let path = dir.path().join("literal.ncraft");
    save(&source(), &path);
    for text in [r#"json:{"answer":42}"#, r#"json:{"type":"text","value":"changed"}"#] {
        let arg = format!("Brief={text}");
        let result = report(cli(&settings, &["run", path.to_str().unwrap(), "--input", &arg, "--out", dir.path().to_str().unwrap(), "--json"]));
        assert_eq!(result["results"][0]["text"], text);
    }
}
