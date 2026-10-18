use nodecraft_format::{decode, encode, journal};
use nodecraft_graph::{Change, Changeset, Project};
use nodecraft_types::CanvasId;
use std::io::{Cursor, Read, Write};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};
const ARCHIVE: &[u8] = include_bytes!("fixtures/v1/comments-v1.ncraft");
const JOURNAL: &[u8] = include_bytes!("fixtures/v1/comments-v1.ncrj");
fn manifest(bytes: &[u8]) -> serde_json::Value {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut json = Vec::new();
    zip.by_name("project.json").unwrap().read_to_end(&mut json).unwrap();
    serde_json::from_slice(&json).unwrap()
}
fn replace_manifest(original: &[u8], json: &serde_json::Value) -> Vec<u8> {
    let mut source = ZipArchive::new(Cursor::new(original)).unwrap();
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    for i in 0..source.len() {
        let mut entry = source.by_index(i).unwrap();
        let name = entry.name().unwrap().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        writer.start_file(&name, options).unwrap();
        if name == "project.json" {
            writer.write_all(&serde_json::to_vec(json).unwrap()).unwrap();
        } else {
            writer.write_all(&bytes).unwrap();
        }
    }
    writer.finish().unwrap().into_inner()
}
#[test]
fn exact_v1_archive_and_original_hash_journal_migrate_losslessly() {
    let snapshot = decode(ARCHIVE).unwrap();
    assert_eq!(snapshot.epoch, 7);
    let recovered = journal::recover(JOURNAL).unwrap();
    assert!(recovered.legacy);
    assert_eq!(recovered.applied, 3);
    assert!(!recovered.truncated_tail);
    let expected: Project = serde_json::from_slice(include_bytes!("fixtures/v1/recovered-v1-project.json")).unwrap();
    assert_eq!(recovered.snapshot.project, expected);
    let v2 = encode(&recovered.snapshot).unwrap();
    assert_eq!(manifest(&v2)["version"], 3);
    assert_eq!(decode(&v2).unwrap(), recovered.snapshot);
    let (_, checkpoint) = journal::checkpoint_from_archive(ARCHIVE.to_vec()).unwrap();
    assert_eq!(&checkpoint.bytes[..8], b"NCRJ0003");
    assert_eq!(manifest(checkpoint.archive().unwrap())["version"], 3);
    let converted = journal::checkpoint(&recovered.snapshot).unwrap();
    let mut bytes = converted.bytes;
    let changes = Changeset::new(vec![Change::SetCanvasTitle { canvas: CanvasId::from_bytes([1; 16]), title: "Migrated".into() }]);
    bytes.extend(journal::frame(converted.identity, 1, &changes, &recovered.snapshot.assets, &Default::default()).unwrap());
    let next = journal::recover(&bytes).unwrap();
    assert!(!next.legacy);
    assert_eq!(next.applied, 1);
    assert!(next.snapshot.project.canvases().values().any(|canvas| canvas.title() == "Migrated"));
}

#[test]
fn v2_writers_round_trip_pins_replies_resolved_notes_and_recovery_rejects_bad_references() {
    use nodecraft_graph::{Comment, CommentReply, Node, Position};
    use nodecraft_types::{CommentId, NodeId, Value};
    let mut snapshot = decode(ARCHIVE).unwrap();
    let scene = snapshot.project.canvases().values().next().unwrap();
    let canvas = scene.id();
    let node = *scene.nodes().keys().next().unwrap();
    let id = CommentId::from_bytes([0x88; 16]);
    let mut root = Comment::new(id, "Review ☀", Position { x: 400.0, y: 80.0 });
    root.node = Some(node);
    root.resolved = true;
    root.replies.push(CommentReply { id: CommentId::from_bytes([0x89; 16]), text: "Flat reply".into() });
    let mut note = Node::new(NodeId::from_bytes([0x90; 16]), "structure.note");
    note.params.insert("text".into(), Value::Text("Local note".into()));
    Changeset::new(vec![Change::PutComment { canvas, comment: root }, Change::AddNode { canvas, node: note }]).apply(&mut snapshot.project).unwrap();
    let bytes = encode(&snapshot).unwrap();
    assert_eq!(decode(&bytes).unwrap(), snapshot);
    let checkpoint = journal::checkpoint(&snapshot).unwrap();
    let changes = Changeset::new(vec![Change::SetCommentResolved { canvas, id, resolved: false }]);
    let mut recovery = checkpoint.bytes.clone();
    recovery.extend(journal::frame(checkpoint.identity, 1, &changes, &snapshot.assets, &Default::default()).unwrap());
    let recovered = journal::recover(&recovery).unwrap();
    let mut expected = snapshot.clone();
    changes.apply(&mut expected.project).unwrap();
    assert_eq!(recovered.snapshot, expected);

    let missing = NodeId::from_bytes([0xee; 16]);
    let mut malformed = manifest(&bytes);
    malformed["project"]["canvases"][canvas.to_string()]["comments"][id.to_string()]["node"] = serde_json::json!(missing);
    assert!(decode(&replace_manifest(&bytes, &malformed)).is_err());
    // A standalone record has no source document; recovery still checks the relation
    // after staging, while live writers must stage before publishing the frame.
    let bad = Changeset::new(vec![Change::SetCommentPin { canvas, id, node: Some(missing), pos: Position::default() }]);
    let mut recovery = checkpoint.bytes;
    recovery.extend(journal::frame(checkpoint.identity, 1, &bad, &snapshot.assets, &Default::default()).unwrap());
    assert!(journal::recover(&recovery).is_err());
}
#[test]
fn v1_headers_reject_v2_fields_and_future_versions_before_typed_graph_decode() {
    let base = manifest(ARCHIVE);
    let node_key = base["project"]["canvases"].as_object().unwrap().values().next().unwrap()["nodes"].as_object().unwrap().keys().next().unwrap().clone();
    let canvas_key = base["project"]["canvases"].as_object().unwrap().keys().next().unwrap().clone();
    for field in ["config", "scalar", "thread"] {
        let mut json = base.clone();
        let canvas = &mut json["project"]["canvases"][&canvas_key];
        match field {
            "config" => canvas["nodes"][&node_key]["config"] = serde_json::json!({}),
            "scalar" => canvas["nodes"][&node_key]["params"]["new"] = serde_json::json!({"type":"bool","value":true}),
            _ => {
                let id = canvas["comments"].as_object().unwrap().keys().next().unwrap().clone();
                canvas["comments"][id]["resolved"] = serde_json::json!(false);
            }
        }
        assert!(decode(&replace_manifest(ARCHIVE, &json)).is_err(), "{field}");
    }
    let mut json = base;
    json["version"] = serde_json::json!(4);
    json["project"] = serde_json::json!({"malformed":true});
    assert!(decode(&replace_manifest(ARCHIVE, &json)).is_err());
    let mut mismatch = JOURNAL.to_vec();
    mismatch[..8].copy_from_slice(b"NCRJ0002");
    assert!(journal::recover(&mismatch).is_err());
}

#[test]
fn omitted_legacy_pos_never_hides_new_thread_fields_and_extensions_keep_old_semantics() {
    let base = manifest(ARCHIVE);
    let canvas_key = base["project"]["canvases"].as_object().unwrap().keys().next().unwrap().clone();
    let root_key = base["project"]["canvases"][&canvas_key]["comments"].as_object().unwrap().keys().next().unwrap().clone();
    for field in ["resolved", "node", "replies"] {
        let mut json = base.clone();
        let root = json["project"]["canvases"][&canvas_key]["comments"][&root_key].as_object_mut().unwrap();
        root.remove("pos");
        root.insert(
            field.into(),
            match field {
                "resolved" => serde_json::json!(true),
                "node" => serde_json::json!(null),
                _ => serde_json::json!([]),
            },
        );
        assert!(decode(&replace_manifest(ARCHIVE, &json)).is_err());
    }
    let mut json = base;
    let node = json["project"]["canvases"][&canvas_key]["nodes"].as_object_mut().unwrap().values_mut().next().unwrap();
    node["unrelated_extension"] =
        serde_json::json!({"kind":"structure.note","config":{},"type":"bool","value":true,"id":"not-a-node","text":"not-a-root","resolved":true});
    assert!(decode(&replace_manifest(ARCHIVE, &json)).is_ok());
}
#[test]
fn checksummed_legacy_put_comment_with_omitted_pos_cannot_smuggle_thread_fields() {
    let checkpoint_len = u64::from_le_bytes(JOURNAL[8..16].try_into().unwrap()) as usize;
    let frame_at = 16 + checkpoint_len;
    let frame_len = u64::from_le_bytes(JOURNAL[frame_at..frame_at + 8].try_into().unwrap()) as usize;
    let payload_at = frame_at + 40;
    let payload = &JOURNAL[payload_at..payload_at + frame_len];
    let json_len = u64::from_le_bytes(payload[..8].try_into().unwrap()) as usize;
    let original: serde_json::Value = serde_json::from_slice(&payload[8..8 + json_len]).unwrap();
    for field in ["resolved", "node", "replies"] {
        let mut record = original.clone();
        let change = record["changes"]["changes"].as_array_mut().unwrap().iter_mut().find(|v| v["type"] == "put_comment").unwrap();
        let root = change["comment"].as_object_mut().unwrap();
        root.remove("pos");
        root.insert(
            field.into(),
            match field {
                "resolved" => serde_json::json!(true),
                "node" => serde_json::json!(null),
                _ => serde_json::json!([]),
            },
        );
        let json = serde_json::to_vec(&record).unwrap();
        let mut replacement = Vec::new();
        replacement.extend_from_slice(&(json.len() as u64).to_le_bytes());
        replacement.extend(json);
        replacement.extend_from_slice(&payload[8 + json_len..]);
        let mut bytes = JOURNAL[..frame_at].to_vec();
        bytes.extend_from_slice(&(replacement.len() as u64).to_le_bytes());
        bytes.extend_from_slice(blake3::hash(&replacement).as_bytes());
        bytes.extend(replacement);
        bytes.extend_from_slice(&JOURNAL[payload_at + frame_len..]);
        assert!(journal::recover(&bytes).is_err(), "{field}");
    }
}

#[test]
fn comments_v2_never_silently_drops_future_tags_or_fields_in_archives_or_records() {
    let v2: &[u8] = include_bytes!("fixtures/v2/comments-v2.ncraft");
    let base = manifest(v2);
    let canvas_key = base["project"]["canvases"].as_object().unwrap().keys().next().unwrap().clone();
    let id = serde_json::to_value(nodecraft_types::TagId::from_bytes([8; 16])).unwrap();
    let key = id.as_str().unwrap();
    for (collection, field, value) in [
        ("tags", "color", serde_json::json!(null)),
        ("fields", "rect", serde_json::json!(null)),
        ("fields", "color", serde_json::json!(null)),
        ("fields", "collapsed", serde_json::json!(false)),
    ] {
        let mut json = base.clone();
        json["project"]["canvases"][&canvas_key][collection][key] = serde_json::json!({"id":id,"title":"Future"});
        json["project"]["canvases"][&canvas_key][collection][key][field] = value.clone();
        assert!(decode(&replace_manifest(v2, &json)).is_err());
        let old_journal = include_bytes!("fixtures/v2/comments-v2.ncrj");
        let archive_len = u64::from_le_bytes(old_journal[8..16].try_into().unwrap()) as usize;
        let checkpoint_bytes = old_journal[..16 + archive_len].to_vec();
        let identity = nodecraft_types::Blake3::from_bytes(*blake3::hash(&checkpoint_bytes[16..]).as_bytes());
        let operation = if collection == "tags" { "put_tag" } else { "put_field" };
        let body = if collection == "tags" { "tag" } else { "field" };
        let mut change = serde_json::json!({"type":operation,"canvas":canvas_key});
        change[body] = serde_json::json!({"id":id,"title":"Future"});
        change[body][field] = value;
        let record = serde_json::json!({"checkpoint":identity,"sequence":1,"changes":{"changes":[change]}});
        let json = serde_json::to_vec(&record).unwrap();
        let mut payload = Vec::new();
        payload.extend_from_slice(&(json.len() as u64).to_le_bytes());
        payload.extend(json);
        payload.extend_from_slice(&0u64.to_le_bytes());
        let mut bytes = checkpoint_bytes;
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(blake3::hash(&payload).as_bytes());
        bytes.extend(payload);
        assert!(journal::recover(&bytes).is_err());
    }
    let mut extension = base;
    extension["project"]["canvases"][&canvas_key]["unrelated_extension"] = serde_json::json!({"tags":{"color":null},"field":{"rect":null,"collapsed":false}});
    assert!(decode(&replace_manifest(v2, &extension)).is_ok());
}

#[test]
fn legacy_headers_also_reject_reserved_v3_roles_in_archive_and_checksummed_frames() {
    let base = manifest(ARCHIVE);
    let canvas_key = base["project"]["canvases"].as_object().unwrap().keys().next().unwrap().clone();
    let id = serde_json::to_value(nodecraft_types::TagId::from_bytes([8; 16])).unwrap();
    let key = id.as_str().unwrap();
    let archive_len = u64::from_le_bytes(JOURNAL[8..16].try_into().unwrap()) as usize;
    let checkpoint = &JOURNAL[..16 + archive_len];
    let identity = nodecraft_types::Blake3::from_bytes(*blake3::hash(&checkpoint[16..]).as_bytes());
    for (collection, field, value) in [
        ("tags", "color", serde_json::json!(null)),
        ("fields", "rect", serde_json::json!(null)),
        ("fields", "color", serde_json::json!(null)),
        ("fields", "collapsed", serde_json::json!(false)),
    ] {
        let mut json = base.clone();
        json["project"]["canvases"][&canvas_key][collection][key] = serde_json::json!({"id":id,"title":"Future"});
        json["project"]["canvases"][&canvas_key][collection][key][field] = value.clone();
        assert!(decode(&replace_manifest(ARCHIVE, &json)).is_err());
        for action in ["put", "add_canvas"] {
            let mut change = if action == "add_canvas" {
                serde_json::json!({"type":"add_canvas","canvas":{"id":canvas_key,"title":"Future","nodes":{},"edges":{},"fields":{},"tags":{}}})
            } else {
                serde_json::json!({"type":if collection=="tags"{"put_tag"}else{"put_field"},"canvas":canvas_key})
            };
            let body = if collection == "tags" { "tag" } else { "field" };
            let entry = serde_json::json!({"id":id,"title":"Future"});
            if action == "add_canvas" {
                change["canvas"][collection][key] = entry;
                change["canvas"][collection][key][field] = value.clone();
            } else {
                change[body] = entry;
                change[body][field] = value.clone();
            }
            let record = serde_json::to_vec(&serde_json::json!({"checkpoint":identity,"sequence":1,"changes":{"changes":[change]}})).unwrap();
            let mut payload = Vec::new();
            payload.extend_from_slice(&(record.len() as u64).to_le_bytes());
            payload.extend(record);
            payload.extend_from_slice(&0u64.to_le_bytes());
            let mut bytes = checkpoint.to_vec();
            bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            bytes.extend_from_slice(blake3::hash(&payload).as_bytes());
            bytes.extend(payload);
            assert!(journal::recover(&bytes).is_err());
        }
    }
}

#[test]
fn frozen_legacy_journals_reject_all_new_bulk_operation_names() {
    for old in [JOURNAL, include_bytes!("fixtures/v2/comments-v2.ncrj").as_slice()] {
        let len = u64::from_le_bytes(old[8..16].try_into().unwrap()) as usize;
        let checkpoint = &old[..16 + len];
        let identity = nodecraft_types::Blake3::from_bytes(*blake3::hash(&checkpoint[16..]).as_bytes());
        for operation in ["set_node_tags", "set_node_fields", "move_nodes"] {
            let record=serde_json::to_vec(&serde_json::json!({"checkpoint":identity,"sequence":1,"changes":{"changes":[{"type":operation,"canvas":CanvasId::from_bytes([1;16]),"assignments":[],"positions":[]}]}})).unwrap();
            let mut payload = Vec::new();
            payload.extend_from_slice(&(record.len() as u64).to_le_bytes());
            payload.extend(record);
            payload.extend_from_slice(&0u64.to_le_bytes());
            let mut bytes = checkpoint.to_vec();
            bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            bytes.extend_from_slice(blake3::hash(&payload).as_bytes());
            bytes.extend(payload);
            assert!(journal::recover(&bytes).is_err(), "{operation}");
        }
    }
}
