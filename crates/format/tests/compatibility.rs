use nodecraft_format::{AssetStore, Snapshot, assets_in_changes, decode, encode, journal, validate_v1_project};
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Node, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, GenerationId, MediaMeta, NodeId, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::io::{Cursor, Write};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
fn canvas() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}
fn node() -> Node {
    Node::new(NodeId::from_bytes([3; 16]), "source.text")
}
fn snapshot_with(node: Node, assets: AssetStore) -> Snapshot {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Frozen v1").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas(), "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: canvas(), node }.apply(&mut project).unwrap();
    Snapshot { project, assets, epoch: 7 }
}
fn archive(project: Json) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file("project.json", SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
    writer.write_all(&serde_json::to_vec(&json!({"version":1,"project":project})).unwrap()).unwrap();
    writer.finish().unwrap().into_inner()
}
// Preserve an actual v1 envelope for contextual refusal tests after current writers advance to v2.
fn legacy_checkpoint(snapshot: &Snapshot) -> journal::Checkpoint {
    let archive = archive(serde_json::to_value(&snapshot.project).unwrap());
    let identity = Blake3::from_bytes(*blake3::hash(&archive).as_bytes());
    let mut bytes = b"NCRJ0001".to_vec();
    bytes.extend_from_slice(&(archive.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&archive);
    journal::Checkpoint { bytes, identity, assets: Default::default() }
}
fn forged_record(checkpoint: &journal::Checkpoint, changes: Json) -> Vec<u8> {
    let json = serde_json::to_vec(&json!({"checkpoint":checkpoint.identity,"sequence":1,"changes":{"changes":changes}})).unwrap();
    let mut payload = (json.len() as u64).to_le_bytes().to_vec();
    payload.extend_from_slice(&json);
    payload.extend_from_slice(&0u64.to_le_bytes());
    let mut bytes = checkpoint.bytes.clone();
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(blake3::hash(&payload).as_bytes());
    bytes.extend(payload);
    bytes
}
fn scalar_values() -> Vec<Value> {
    [json!({"type":"bool","value":true}), json!({"type":"integer","value":"-3"}), json!({"type":"unsigned","value":"3"}), json!({"type":"number","value":1.5})]
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect()
}
#[test]
fn genuine_v1_frames_use_original_identity_and_current_checkpoint_migrates_once() {
    let archive = include_bytes!("fixtures/v1/comments-v1.ncraft");
    let journal_bytes = include_bytes!("fixtures/v1/comments-v1.ncrj");
    let loaded = decode(archive).unwrap();
    assert_eq!(decode(&encode(&loaded).unwrap()).unwrap(), loaded);
    let checkpoint = journal::checkpoint_from_archive(archive.to_vec()).unwrap().1;
    assert_ne!(checkpoint.archive().unwrap(), archive);
    assert_ne!(checkpoint.identity, Blake3::from_bytes(*blake3::hash(archive).as_bytes()));
    assert_eq!(checkpoint.bytes.get(..8), Some(b"NCRJ0003".as_slice()));
    assert!(journal::recover(journal_bytes).unwrap().legacy);
    let recovered = journal::recover(journal_bytes).unwrap();
    assert_eq!(recovered.applied, 3);
    assert!(!recovered.truncated_tail);
    let expected: Project = serde_json::from_slice(include_bytes!("fixtures/v1/recovered-v1-project.json")).unwrap();
    assert_eq!(recovered.snapshot.project, expected);
    assert_eq!(journal::recover(&journal::checkpoint(&recovered.snapshot).unwrap().bytes).unwrap().snapshot, recovered.snapshot);
}
#[test]
fn current_writers_admit_new_values_while_v1_envelopes_refuse_every_replay_role() {
    let assets = AssetStore::default();
    for value in scalar_values().into_iter().map(|v| Value::List(vec![Value::List(vec![v])])) {
        let mut n = node();
        n.params.insert("value".into(), value.clone());
        let snapshot = snapshot_with(n.clone(), assets.clone());
        assert_eq!(validate_v1_project(&snapshot.project).unwrap_err().kind, ErrorKind::Unsupported);
        assert_eq!(decode(&encode(&snapshot).unwrap()).unwrap(), snapshot);
        assert!(journal::checkpoint(&snapshot).is_ok());
        let generation = Generation { id: GenerationId::from_bytes([9; 16]), outputs: vec![value.clone()], input_hash: None };
        let mut canvas_data = Canvas::new(canvas(), "Canvas");
        let mut p = Project::new(ProjectId::from_bytes([1; 16]), "P").unwrap();
        Change::AddCanvas { canvas: canvas_data.clone() }.apply(&mut p).unwrap();
        Change::AddNode { canvas: canvas(), node: n.clone() }.apply(&mut p).unwrap();
        canvas_data = p.canvases().get(&canvas()).unwrap().clone();
        for change in [
            Change::AddCanvas { canvas: canvas_data },
            Change::AddNode { canvas: canvas(), node: n.clone() },
            Change::SetNode { canvas: canvas(), node: n.clone() },
            Change::RestoreNode { canvas: canvas(), node: n, edges: vec![], pins: vec![] },
            Change::SetParam { canvas: canvas(), id: node().id, name: "scalar".into(), value: Some(value) },
            Change::AddGeneration { canvas: canvas(), id: node().id, index: 0, generation: generation.clone() },
        ] {
            let changes = Changeset::new(vec![change]);
            assert!(journal::validate_replay(&changes, &assets).is_ok());
            assert!(journal::frame(Blake3::from_bytes([0; 32]), 1, &changes, &assets, &Default::default()).is_ok());
            let checkpoint = legacy_checkpoint(&snapshot_with(node(), AssetStore::default()));
            let raw = serde_json::to_value(&changes).unwrap();
            assert_eq!(journal::recover(&forged_record(&checkpoint, raw["changes"].clone())).err().unwrap().kind, ErrorKind::Unsupported);
        }
        let mut n = node();
        n.history.push(generation);
        let snapshot = snapshot_with(n, assets.clone());
        assert!(validate_v1_project(&snapshot.project).is_err());
        assert_eq!(decode(&encode(&snapshot).unwrap()).unwrap(), snapshot);
    }
    let mut n = node();
    n.config.insert("text-only".into(), Value::Text("PRIVATE_VALUE".into()));
    let snapshot = snapshot_with(n, assets);
    assert_eq!(decode(&encode(&snapshot).unwrap()).unwrap(), snapshot);
    let error = validate_v1_project(&snapshot.project).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(!error.to_string().contains("PRIVATE_VALUE"));
}
#[test]
fn raw_v1_guard_rejects_reserved_roles_but_tolerates_unrelated_extension_names() {
    let base = snapshot_with(node(), AssetStore::default());
    let project = serde_json::to_value(&base.project).unwrap();
    let checkpoint = legacy_checkpoint(&base);
    let c = canvas().to_string();
    let id = node().id.to_string();
    for reserved in [json!({}), Json::Null, json!({"text":{"type":"text","value":"x"}})] {
        let mut bad = project.clone();
        bad["canvases"][&c]["nodes"][&id]["config"] = reserved.clone();
        assert_eq!(decode(&archive(bad)).unwrap_err().kind, ErrorKind::Unsupported);
        let mut n = serde_json::to_value(node()).unwrap();
        n["config"] = reserved;
        for kind in ["add_node", "set_node", "restore_node"] {
            let bytes = forged_record(&checkpoint, json!([{"type":kind,"canvas":canvas(),"node":n,"edges":[]}]));
            assert_eq!(journal::recover(&bytes).err().unwrap().kind, ErrorKind::Unsupported);
        }
    }
    let comment = json!({"id":nodecraft_types::CommentId::from_bytes([5;16]),"text":"legacy"});
    let field = json!({"id":nodecraft_types::FieldId::from_bytes([6;16]),"title":"legacy"});
    let tag = json!({"id":nodecraft_types::TagId::from_bytes([7;16]),"title":"legacy"});
    for (role, original, keys, op) in [
        ("comments", comment, vec!["node", "replies", "resolved"], "put_comment"),
        ("fields", field, vec!["rect", "color", "collapsed"], "put_field"),
        ("tags", tag, vec!["color"], "put_tag"),
    ] {
        for key in keys {
            let mut data = original.clone();
            data[key] = Json::Null;
            let mut bad = project.clone();
            bad["canvases"][&c][role] = json!({data["id"].as_str().unwrap():data.clone()});
            assert_eq!(decode(&archive(bad)).unwrap_err().kind, ErrorKind::Unsupported);
            let mut change = json!({"type":op,"canvas":canvas()});
            change[match role {
                "comments" => "comment",
                "fields" => "field",
                _ => "tag",
            }] = data;
            assert_eq!(journal::recover(&forged_record(&checkpoint, json!([change]))).err().unwrap().kind, ErrorKind::Unsupported);
        }
    }
    for value in scalar_values() {
        let mut bad = project.clone();
        bad["canvases"][&c]["nodes"][&id]["params"]["scalar"] = serde_json::to_value(&value).unwrap();
        assert_eq!(decode(&archive(bad)).unwrap_err().kind, ErrorKind::Unsupported);
        for change in [
            json!({"type":"set_param","canvas":canvas(),"id":node().id,"name":"x","value":value}),
            json!({"type":"add_generation","canvas":canvas(),"id":node().id,"index":0,"generation":{"id":GenerationId::from_bytes([8;16]),"outputs":[value]}}),
        ] {
            assert_eq!(journal::recover(&forged_record(&checkpoint, json!([change]))).err().unwrap().kind, ErrorKind::Unsupported);
        }
    }
    let mut note = project.clone();
    note["canvases"][&c]["nodes"][&id]["kind"] = json!("structure.note");
    assert_eq!(decode(&archive(note)).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(journal::recover(&forged_record(&checkpoint, json!([{"type":"set_comment_resolved"}]))).err().unwrap().kind, ErrorKind::Unsupported);
    let mut harmless = project.clone();
    harmless["extension"] = json!({"kind":"structure.note","type":"bool","config":{},"replies":[]});
    harmless["canvases"][&c]["nodes"][&id]["extension"] = json!({"config":{},"type":"number"});
    assert_eq!(decode(&archive(harmless)).unwrap().project, base.project);
    let bytes =
        forged_record(&checkpoint, json!([{"type":"set_node_title","canvas":canvas(),"id":node().id,"title":"new","extension":{"config":{},"type":"number"}}]));
    assert_eq!(journal::recover(&bytes).unwrap().applied, 1);
    assert!(!serde_json::to_string(&base.project).unwrap().contains("config"));
}
#[test]
fn config_media_collectors_validate_all_node_roles_even_though_v1_writes_refuse_config() {
    let mut assets = AssetStore::default();
    let hash = assets.insert(b"configuration media").unwrap();
    let media = Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() });
    let mut n = node();
    n.config.insert("nested".into(), Value::List(vec![Value::List(vec![media.clone()])]));
    let snapshot = snapshot_with(n.clone(), assets.clone());
    assert_eq!(assets.validate_project(&snapshot.project).unwrap(), [hash].into_iter().collect());
    assert!(AssetStore::default().validate_project(&snapshot.project).is_err());
    let mut wrong = n.clone();
    wrong.config.insert("wrong".into(), Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() }));
    assert!(assets.validate_project(&snapshot_with(wrong, assets.clone()).project).is_err());
    let canvas_data = snapshot.project.canvases().get(&canvas()).unwrap().clone();
    for change in [
        Change::AddCanvas { canvas: canvas_data },
        Change::AddNode { canvas: canvas(), node: n.clone() },
        Change::SetNode { canvas: canvas(), node: n.clone() },
        Change::RestoreNode { canvas: canvas(), node: n, edges: vec![], pins: vec![] },
    ] {
        assert_eq!(assets_in_changes(&Changeset::new(vec![change])).unwrap(), [hash].into_iter().collect());
    }
    let mut project = snapshot.project.clone();
    let inverse = Change::RemoveNode { canvas: canvas(), id: node().id }.apply(&mut project).unwrap();
    assert_eq!(assets_in_changes(&inverse).unwrap(), [hash].into_iter().collect());
    assert!(validate_v1_project(&snapshot.project).is_err());
}

#[test]
fn config_node_frames_and_future_generation_hashes_never_acquire_v1_meaning() {
    let mut n = node();
    n.config.insert("text-only".into(), Value::Text("text".into()));
    let snapshot = snapshot_with(n.clone(), AssetStore::default());
    let c = snapshot.project.canvases().get(&canvas()).unwrap().clone();
    for change in [
        Change::AddCanvas { canvas: c },
        Change::AddNode { canvas: canvas(), node: n.clone() },
        Change::SetNode { canvas: canvas(), node: n.clone() },
        Change::RestoreNode { canvas: canvas(), node: n, edges: vec![], pins: vec![] },
    ] {
        let changes = Changeset::new(vec![change]);
        assert!(journal::validate_replay(&changes, &snapshot.assets).is_ok());
        assert!(journal::frame(Blake3::from_bytes([0; 32]), 1, &changes, &snapshot.assets, &Default::default()).is_ok());
        let checkpoint = legacy_checkpoint(&snapshot_with(node(), AssetStore::default()));
        let raw = serde_json::to_value(&changes).unwrap();
        assert_eq!(journal::recover(&forged_record(&checkpoint, raw["changes"].clone())).err().unwrap().kind, ErrorKind::Unsupported);
    }
    let snapshot = snapshot_with(node(), AssetStore::default());
    let checkpoint = legacy_checkpoint(&snapshot);
    for reserved in [Json::Null, json!(Blake3::from_bytes([1; 32]))] {
        let generation = json!({"id":GenerationId::from_bytes([9;16]),"outputs":[],"input_hash":reserved});
        let mut project = serde_json::to_value(&snapshot.project).unwrap();
        project["canvases"][canvas().to_string()]["nodes"][node().id.to_string()]["history"] = json!([generation]);
        assert_eq!(decode(&archive(project)).unwrap_err().kind, ErrorKind::Unsupported);
        let change = json!({"type":"add_generation","canvas":canvas(),"id":node().id,"index":0,"generation":generation});
        assert_eq!(journal::recover(&forged_record(&checkpoint, json!([change]))).err().unwrap().kind, ErrorKind::Unsupported);
    }
}

#[test]
fn typed_generation_hashes_require_v2_but_absent_hash_stays_genuine_v1() {
    let mut generation = Generation { id: GenerationId::from_bytes([9; 16]), outputs: vec![Value::Text("legacy".into())], input_hash: None };
    assert!(serde_json::to_value(&generation).unwrap().get("input_hash").is_none());
    let mut n = node();
    n.history.push(generation.clone());
    let legacy = snapshot_with(n, AssetStore::default());
    assert!(validate_v1_project(&legacy.project).is_ok());
    assert_eq!(decode(&encode(&legacy).unwrap()).unwrap(), legacy);
    generation.input_hash = Some(Blake3::from_bytes([1; 32]));
    let mut n = node();
    n.history.push(generation.clone());
    let hashed = snapshot_with(n.clone(), AssetStore::default());
    assert_eq!(validate_v1_project(&hashed.project).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(decode(&encode(&hashed).unwrap()).unwrap(), hashed);
    let canvas_data = hashed.project.canvases().get(&canvas()).unwrap().clone();
    for change in [
        Change::AddCanvas { canvas: canvas_data },
        Change::AddNode { canvas: canvas(), node: n.clone() },
        Change::SetNode { canvas: canvas(), node: n.clone() },
        Change::RestoreNode { canvas: canvas(), node: n, edges: vec![], pins: vec![] },
        Change::AddGeneration { canvas: canvas(), id: node().id, index: 0, generation },
    ] {
        let changes = Changeset::new(vec![change]);
        assert!(journal::validate_replay(&changes, &hashed.assets).is_ok());
        assert!(journal::frame(Blake3::from_bytes([0; 32]), 1, &changes, &hashed.assets, &Default::default()).is_ok());
        let checkpoint = legacy_checkpoint(&snapshot_with(node(), AssetStore::default()));
        let raw = serde_json::to_value(&changes).unwrap();
        assert_eq!(journal::recover(&forged_record(&checkpoint, raw["changes"].clone())).err().unwrap().kind, ErrorKind::Unsupported);
    }
}
