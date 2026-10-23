use super::*;
use crate::impact::Impact;

mod persistence;

fn params(page: u64) -> Json {
    json!({"canvas":C,"node":SOURCE_ID,"page":page,"id":NEW,"at":AT})
}

#[test]
fn commands_publish_one_edit_under_their_own_id_and_restore_it_with_undo_redo() {
    for command in ["document.extract_page", "document.extract_text"] {
        let mut s = session();
        let source = node(&s, SOURCE_ID).unwrap().clone();
        let revision = s.revision();
        let before = s.document().clone();
        let media = s.assets().len();
        let mut args = params(2);
        if command.ends_with("text") {
            args["region"] = json!([0.0, 0.0, 0.5, 0.5]);
        }
        assert!(s.enabled(command, &args).is_ok());
        assert_eq!(s.impact(command, &args).unwrap(), Impact::Edit);
        assert_eq!(s.document(), &before);
        assert_eq!(s.assets().len(), media);
        assert_eq!(s.revision(), revision);
        assert_eq!(s.dispatch(command, &args).unwrap(), json!({"id":NEW}));
        assert_eq!(s.revision(), revision + 1);
        let events = s.events_since(revision).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events.first().unwrap().command, command);
        let made = node(&s, NEW).unwrap().clone();
        assert_eq!(made.pos, AT);
        assert_eq!(node(&s, SOURCE_ID), Some(&source));
        if command.ends_with("text") {
            assert_eq!(made.kind, "source.text");
            assert_eq!(made.params.get("text"), Some(&Value::Text("Top left".into())));
            assert_eq!(s.assets().len(), media);
        } else {
            assert_eq!(made.kind, SOURCE);
            let Some(Value::Media(asset)) = made.params.get("asset") else { panic!("an extracted PDF") };
            assert_eq!(asset.meta.page_count, Some(1));
            let bytes = s
                .assets()
                .open_verified(asset, &TransferCancel::default(), &StreamMetrics::default())
                .unwrap()
                .read_small(MAX_ASSET_BYTES, &StreamMetrics::default())
                .unwrap();
            let copied = Document::open(&bytes, &DocumentLimits::default()).unwrap();
            assert_eq!(copied.page_count(), 1);
            assert_eq!(copied.text(0).unwrap(), "Top left\nBottom right");
        }
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
        s.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(node(&s, NEW), Some(&made));
        assert_eq!(node(&s, SOURCE_ID), Some(&source));
    }
}

#[test]
fn omitted_ids_are_fresh_and_placement_is_relative_to_the_source() {
    let mut s = session();
    let args = json!({"canvas":C,"node":SOURCE_ID,"page":1});
    let mut ids = Vec::new();
    for _ in 0..2 {
        let value = s.dispatch("document.extract_text", &args).unwrap();
        let id: NodeId = serde_json::from_value(value["id"].clone()).unwrap();
        assert_eq!(id.to_string().as_bytes().get(14), Some(&b'4'));
        assert_ne!(id, SOURCE_ID);
        assert!(!ids.contains(&id));
        let made = node(&s, id).unwrap();
        assert_eq!(made.pos, Position { x: 300.0, y: 0.0 });
        assert_eq!(made.params.get("text"), Some(&Value::Text("Cover".into())));
        ids.push(id);
    }
}

#[test]
fn hostile_command_parameters_refuse_before_any_document_media_or_history_change() {
    let mut s = session();
    let non_pdf = NodeId::from_bytes([6; 16]);
    s.dispatch("node.create", &json!({"canvas":C,"id":non_pdf,"kind":"source.text","pos":{"x":0,"y":0}})).unwrap();
    let mut cases = Vec::new();
    for (field, value) in [
        ("page", json!(0)),
        ("page", json!(4)),
        ("page", json!(u64::MAX)),
        ("page", json!(1.5)),
        ("page", json!("2")),
        ("node", json!(NEW)),
        ("node", json!(non_pdf)),
        ("canvas", json!(CanvasId::from_bytes([77; 16]))),
        ("id", json!(SOURCE_ID)),
        ("at", json!({"x":1e100,"y":0})),
        ("at", json!({"x":0,"y":null})),
        ("at", json!({"x":0,"y":0,"unknown":true})),
        ("unknown", json!(true)),
    ] {
        let mut args = params(1);
        args[field] = value;
        cases.push(args);
    }
    for region in [
        json!([0, 0, 1]),
        json!([0, 0, 1, 1, 1]),
        json!([1, 0, 0, 1]),
        json!([0, 1, 1, 0]),
        json!([0, 0, 0, 1]),
        json!([-0.1, 0, 1, 1]),
        json!([0, 0, 1.1, 1]),
        json!([null, 0, 1, 1]),
        json!(["bad", 0, 1, 1]),
        json!([0, 0, 1e100, 1]),
    ] {
        let mut args = params(1);
        args["region"] = region;
        cases.push(args);
    }
    let before = s.document().clone();
    let revision = s.revision();
    let assets = s.assets().len();
    let events = s.events_since(0).unwrap();
    for command in ["document.extract_page", "document.extract_text"] {
        for args in &cases {
            assert!(s.enabled(command, args).is_err(), "{command}: {args}");
            assert!(s.impact(command, args).is_err(), "{command}: {args}");
            assert!(s.dispatch(command, args).is_err(), "{command}: {args}");
            assert_eq!(s.document(), &before);
            assert_eq!(s.revision(), revision);
            assert_eq!(s.assets().len(), assets);
            assert_eq!(s.events_since(0).unwrap(), events);
        }
    }
    let mut region_on_page = params(1);
    region_on_page["region"] = json!([0, 0, 1, 1]);
    assert!(s.dispatch("document.extract_page", &region_on_page).is_err());
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(node(&s, non_pdf).is_none(), "refusals did not add an undo step");
}

#[test]
fn empty_text_regions_and_scanned_pages_leave_no_partial_import_or_edit() {
    let mut s = session();
    let before = s.document().clone();
    let revision = s.revision();
    let count = s.assets().len();
    let mut empty = params(2);
    empty["region"] = json!([0.4, 0.4, 0.45, 0.45]);
    for args in [params(3), empty] {
        assert!(s.enabled("document.extract_text", &args).is_ok(), "preview does not parse page contents");
        let error = s.dispatch("document.extract_text", &args).unwrap_err();
        assert!(error.message.contains("no text"), "{}", error.message);
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert_eq!(s.assets().len(), count);
    }
}

#[test]
fn extraction_schemas_describe_one_based_pages_and_bounded_regions() {
    let s = session();
    for id in ["document.extract_page", "document.extract_text"] {
        let schema = (s.registry().get(id).unwrap().input_schema)();
        assert_eq!(schema["properties"]["page"]["minimum"], 1);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["canvas", "node", "page"]));
        assert_eq!(schema["properties"].get("region").is_some(), id.ends_with("text"));
    }
}

#[test]
fn viewer_and_commenter_roles_cannot_begin_extraction_or_report_it_ready() {
    for role in [nodecraft_sync::Role::View, nodecraft_sync::Role::Comment] {
        let mut s = session();
        s.set_access(crate::access::Access::new(role, Some("Public reviewer".into())).unwrap());
        let before = s.document().clone();
        let revision = s.revision();
        let assets = s.assets().len();
        for command in ["document.extract_page", "document.extract_text"] {
            let args = params(1);
            assert_eq!(s.enabled(command, &args).unwrap_err().kind, ErrorKind::Forbidden);
            assert_eq!(s.impact(command, &args).unwrap_err().kind, ErrorKind::Forbidden);
            assert_eq!(s.dispatch(command, &args).unwrap_err().kind, ErrorKind::Forbidden);
            assert_eq!(s.document(), &before);
            assert_eq!(s.revision(), revision);
            assert_eq!(s.assets().len(), assets);
        }
    }
}

#[test]
fn unreadable_pdf_contents_are_only_opened_on_dispatch_and_leave_no_partial_edit() {
    let mut s = session();
    let Some(Value::Media(mut asset)) = node(&s, SOURCE_ID).unwrap().params.get("asset").cloned() else { panic!("PDF fixture") };
    asset.hash = s.import_asset(b"%PDF-1.7\nnot a valid document\n%%EOF\n").unwrap();
    s.dispatch("node.param_set", &json!({"canvas":C,"id":SOURCE_ID,"name":"asset","value":Value::Media(asset)})).unwrap();
    let before = s.document().clone();
    let revision = s.revision();
    let assets = s.assets().len();
    for command in ["document.extract_page", "document.extract_text"] {
        assert!(s.enabled(command, &params(1)).is_ok());
        assert_eq!(s.impact(command, &params(1)).unwrap(), Impact::Edit);
        assert!(s.dispatch(command, &params(1)).is_err());
        assert_eq!(s.document(), &before);
        assert_eq!(s.revision(), revision);
        assert_eq!(s.assets().len(), assets);
        assert!(s.events_since(revision).unwrap().is_empty());
    }
}
