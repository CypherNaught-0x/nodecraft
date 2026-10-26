//! The command contract for batch local export (#98): what the palette, CLI, control channel and an
//! agent in the canvas can ask for, what they are refused, and what never happens — no document
//! edit, no dialog, no write into a folder the app did not authorize.
//!
//! The fixtures hold **media** results, because `history.export` (#557) refuses a text result and
//! this command reuses that refusal; [`a_text_result_is_refused_the_way_557_refuses_it`] pins that.
use crate::Session;
use crate::local_export::{ExportHost, MAX_ROOTS, MAX_SOURCES};
use nodecraft_format::export::{Collision, reasons};
use nodecraft_graph::{Canvas, Change, Generation, Node, Port, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, GenerationId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([0x98; 16]);
const HERO: &[u8] = b"\x89PNG the active result, not really a PNG";
const WIDE: &[u8] = b"\x89PNG the second result, also not really";
#[cfg(unix)]
const OLDER: &[u8] = b"\x89PNG the older take";

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn media(bytes: &[u8]) -> Value {
    let asset = AssetRef { hash: Blake3::from_bytes(*blake3::hash(bytes).as_bytes()), mime: "image/png".parse().expect("mime"), meta: Default::default() };
    Value::Media(asset)
}

fn image_port(name: &str, many: bool) -> Port {
    Port { name: name.to_owned(), port_type: PortType { modality: Modality::Image, many } }
}

/// A node with one image result in use, titled `title`.
fn ran(n: u8, title: &str, bytes: &[u8]) -> Node {
    let mut node = Node::new(id(n), "test.gen");
    node.title = title.to_owned();
    node.outputs = vec![image_port("out", false)];
    let result = Generation::new(GenerationId::from_bytes([n; 16]), vec![media(bytes)]);
    node.active = Some(result.id);
    node.history.push(result);
    node
}

/// `ran`, plus an older result sitting behind it in history.
#[cfg(unix)]
fn ran_twice(n: u8, title: &str, older: &[u8], newer: &[u8]) -> Node {
    let mut node = ran(n, title, newer);
    node.history.insert(0, Generation::new(GenerationId::from_bytes([n ^ 0x55; 16]), vec![media(older)]));
    node
}

/// A node with two results on one list output.
fn ran_list(n: u8, title: &str, first: &[u8], second: &[u8]) -> Node {
    let mut node = Node::new(id(n), "test.gen");
    node.title = title.to_owned();
    node.outputs = vec![image_port("out", true)];
    let result = Generation::new(GenerationId::from_bytes([n; 16]), vec![Value::List(vec![media(first), media(second)])]);
    node.active = Some(result.id);
    node.history.push(result);
    node
}

/// A node whose active result is text. #557 refuses these, and so does this.
fn ran_text(n: u8, title: &str, text: &str) -> Node {
    let mut node = Node::new(id(n), "test.gen");
    node.title = title.to_owned();
    node.outputs = vec![Port { name: "out".to_owned(), port_type: PortType { modality: Modality::Text, many: false } }];
    let result = Generation::new(GenerationId::from_bytes([n; 16]), vec![Value::Text(text.to_owned())]);
    node.active = Some(result.id);
    node.history.push(result);
    node
}

/// A node that has never been run.
fn idle(n: u8) -> Node {
    let mut node = Node::new(id(n), "test.gen");
    node.outputs = vec![image_port("out", false)];
    node
}

/// A session whose store holds the bytes of `bytes`, in the order the nodes were built.
fn session_with(nodes: Vec<Node>, bytes: &[&[u8]]) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0x98; 16]), "Export").expect("project");
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).expect("canvas");
    for node in nodes {
        Change::AddNode { canvas: C, node }.apply(&mut project).expect("node");
    }
    let mut session = Session::new(project).expect("session");
    for bytes in bytes {
        session.import_asset(bytes).expect("import");
    }
    session
}

fn session() -> Session {
    session_with(vec![ran(1, "hero", HERO), ran(2, "wide", WIDE), idle(3), ran_text(4, "words", "just words")], &[HERO, WIDE])
}

fn request(sources: Json, destination: &std::path::Path) -> Json {
    json!({"canvas": C, "sources": sources, "destination": destination.display().to_string()})
}

fn sources(items: &[Json]) -> Json {
    Json::Array(items.to_vec())
}

fn names(result: &Json) -> Vec<String> {
    result["entries"].as_array().expect("entries").iter().filter_map(|entry| entry["file"].as_str().map(str::to_owned)).collect()
}

#[cfg(unix)]
fn reasons_of(result: &Json) -> Vec<String> {
    result["entries"].as_array().expect("entries").iter().filter_map(|entry| entry["reason"].as_str().map(str::to_owned)).collect()
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).expect("read dir").filter_map(|entry| entry.ok()).map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn a_preview_names_every_file_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let params = request(sources(&[json!({"node": id(1), "port": "out"}), json!({"node": id(2), "port": "out"})]), dir.path());

    let preview = s.dispatch("export.local_plan", &params).expect("preview");
    assert_eq!(preview["entries"].as_array().expect("entries").len(), 2);
    assert_eq!(names(&preview), vec!["hero-out-1.png".to_owned(), "wide-out-2.png".to_owned()]);
    assert_eq!(preview["entries"][0]["state"], "planned");
    assert_eq!(preview["entries"][0]["action"], "passthrough", "a preview never claims a conversion");
    assert_eq!(preview["entries"][0]["mime"], "image/png", "the extension comes from the stored type");
    assert_eq!(preview["entries"][0]["bytes"], json!(HERO.len()));
    assert_eq!(preview["entries"][0]["port"], "out");
    assert_eq!(preview["entries"][0]["node"], id(1).to_string());
    assert_eq!(preview["entries"][0]["generation"], GenerationId::from_bytes([1; 16]).to_string(), "the file names the generation it came from");
    assert!(preview["entries"][0]["source_id"].as_str().expect("id").contains(&id(1).to_string()));
    assert_eq!(preview["on_collision"], "skip", "the default leaves an existing file alone");
    assert_eq!(preview["format_mode"], "original");
    assert!(preview["capability"].as_str().expect("capability").contains("pass-through"));
    assert_eq!(files_in(dir.path()), Vec::<String>::new(), "a preview writes nothing");
    assert_eq!(preview.get("destination"), None, "a folder path is not echoed back through a result");
}

#[cfg(not(unix))]
#[test]
fn an_unsupported_run_neither_creates_nor_replaces_files() {
    let dir = tempfile::tempdir().expect("temp");
    let existing = dir.path().join("hero-out-1.png");
    let sentinel = dir.path().join("other.png");
    std::fs::write(&sentinel, b"leave this alone").expect("sentinel");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let mut params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());
    assert_eq!(s.dispatch("export.local_plan", &params).expect("read-only plan")["entries"][0]["state"], "planned");
    for (policy, contents) in [("skip", None), ("overwrite", Some(b"preserve my original".as_slice()))] {
        if let Some(contents) = contents {
            std::fs::write(&existing, contents).expect("existing file");
        }
        params["on_collision"] = json!(policy);
        for error in [s.enabled("export.local_run", &params).unwrap_err(), s.dispatch("export.local_run", &params).unwrap_err()] {
            assert_eq!(error.kind, ErrorKind::Unsupported);
            assert!(error.message.contains("descriptor-relative no-follow"), "{error:?}");
        }
        if let Some(contents) = contents {
            assert_eq!(std::fs::read(&existing).expect("unchanged file"), contents);
        } else {
            assert!(!existing.exists(), "refused run must not create a file");
        }
        assert_eq!(std::fs::read(&sentinel).expect("unchanged sentinel"), b"leave this alone");
    }
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png", "other.png"]);
}

#[cfg(unix)]
#[test]
fn running_writes_the_active_result_of_each_source_and_reports_every_entry() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let params = request(sources(&[json!({"node": id(1), "port": "out"}), json!({"node": id(2), "port": "out"})]), dir.path());

    let preview = s.dispatch("export.local_plan", &params).expect("preview");
    let run = s.dispatch("export.local_run", &params).expect("run");
    assert_eq!(run["complete"], true);
    assert_eq!(run["counts"]["written"], 2);
    assert_eq!(run["partial"], false);
    assert_eq!(run["cancelled"], false);
    assert_eq!(names(&run), names(&preview), "the preview and the run agree on every name");
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned(), "wide-out-2.png".to_owned()]);
    assert_eq!(std::fs::read(dir.path().join("hero-out-1.png")).expect("read"), HERO, "the stored bytes, byte for byte");
    assert_eq!(run["entries"][0]["state"], "written");
    assert_eq!(run["entries"][0]["bytes"], json!(HERO.len()));
}

#[cfg(unix)]
#[test]
fn the_active_generation_is_the_payload_and_history_is_left_in_the_project() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session_with(vec![ran_twice(1, "hero", OLDER, HERO)], &[HERO, OLDER]);
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());

    let run = s.dispatch("export.local_run", &params).expect("run");
    assert_eq!(run["counts"]["written"], 1, "one active result, not one file per generation");
    assert_eq!(std::fs::read(dir.path().join("hero-out-1.png")).expect("read"), HERO);
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned()]);

    // The person chooses the older result: the next export writes that one, and still only one file.
    let older = GenerationId::from_bytes([1 ^ 0x55; 16]);
    s.dispatch("history.select", &json!({"canvas": C, "id": id(1), "generation": older})).expect("select");
    let again = s
        .dispatch(
            "export.local_run",
            &json!({
                "canvas": C,
                "sources": [json!({"node": id(1), "port": "out"})],
                "destination": dir.path().display().to_string(),
                "on_collision": "overwrite"
            }),
        )
        .expect("second run");
    assert_eq!(again["entries"][0]["generation"], older.to_string());
    assert_eq!(std::fs::read(dir.path().join("hero-out-1.png")).expect("read"), OLDER);
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned()], "history is not exported");
}

#[cfg(unix)]
#[test]
fn an_export_changes_no_document_and_no_undo_state() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let before = s.document().clone();
    let revision = s.revision();
    let can_undo = s.can_undo();

    let run = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path())).expect("run");
    assert_eq!(run["counts"]["written"], 1, "the write happened");
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned()]);
    assert_eq!(*s.document(), before, "export is not a document edit");
    assert_eq!(s.revision(), revision);
    assert_eq!(s.can_undo(), can_undo, "an export is not an undo step");
}

#[test]
fn nothing_is_offered_where_no_folder_has_been_authorized() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    let params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());
    for id in ["export.local_plan", "export.local_run"] {
        let error = s.enabled(id, &params).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert_eq!(error.message, reasons::NO_HOST, "the reason says what to do about it");
        let error = s.dispatch(id, &params).unwrap_err();
        assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Unsupported, reasons::NO_HOST));
    }
    assert_eq!(files_in(dir.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_folder_outside_the_authorized_one_is_refused_even_when_named_from_inside_it() {
    let inside = tempfile::tempdir().expect("temp");
    let outside = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(inside.path()).expect("authorize")));
    let node = json!({"node": id(1), "port": "out"});

    // Named directly.
    let error = s.dispatch("export.local_run", &request(sources(std::slice::from_ref(&node)), outside.path())).unwrap_err();
    assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Unsupported, reasons::UNSUPPORTED_TARGET));
    // Parent traversal is refused even before folder-authority resolution.
    let sneaky = inside.path().join("..").join(outside.path().file_name().expect("name"));
    let error = s.dispatch("export.local_run", &request(sources(std::slice::from_ref(&node)), &sneaky)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("'..'"));
    let still_inside = inside.path().join("shots").join("..");
    std::fs::create_dir(inside.path().join("shots")).expect("subfolder");
    let error = s.dispatch("export.local_run", &request(sources(std::slice::from_ref(&node)), &still_inside)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput, "an in-root parent segment is still unsafe input");
    assert!(error.message.contains("'..'"));
    // A relative path is refused rather than resolved against whatever the process happens to use.
    let error = s.dispatch("export.local_run", &json!({"canvas": C, "sources": [node], "destination": "."}));
    assert_eq!(error.unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(files_in(outside.path()), Vec::<String>::new());
    assert_eq!(files_in(inside.path()), vec!["shots".to_owned()], "refused requests must not write files");
}

#[cfg(unix)]
#[test]
fn a_subfolder_of_an_authorized_folder_is_allowed_because_it_is_inside_it() {
    let root = tempfile::tempdir().expect("temp");
    let sub = root.path().join("shots");
    std::fs::create_dir(&sub).expect("create");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(root.path()).expect("authorize")));
    let run = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), &sub)).expect("inside is inside");
    assert_eq!(run["counts"]["written"], 1);
    assert_eq!(files_in(&sub), vec!["hero-out-1.png".to_owned()]);
}

#[cfg(unix)]
#[test]
fn overwrite_does_not_follow_an_existing_file_link_outside_the_authorized_folder() {
    let dir = tempfile::tempdir().expect("folder");
    let outside = tempfile::tempdir().expect("outside");
    let other = outside.path().join("private.png");
    std::fs::write(&other, b"untouched").expect("original");
    std::os::unix::fs::symlink(&other, dir.path().join("hero-out-1.png")).expect("link");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let mut params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());
    params["on_collision"] = json!("overwrite");
    let result = s.dispatch("export.local_run", &params).expect("entry failure, not a crash");
    assert_eq!(result["entries"][0]["state"], "failed");
    assert!(result["entries"][0]["reason"].as_str().expect("reason").contains("symbolic link"));
    assert_eq!(std::fs::read(&other).expect("outside"), b"untouched");
}

#[cfg(unix)]
#[test]
fn a_planted_dangling_file_link_cannot_create_a_file_outside_the_grant() {
    let dir = tempfile::tempdir().expect("folder");
    let outside = tempfile::tempdir().expect("outside");
    let private = outside.path().join("missing.png");
    std::os::unix::fs::symlink(&private, dir.path().join("hero-out-1.png")).expect("link");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let mut params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());
    for policy in ["skip", "numbered", "overwrite"] {
        params["on_collision"] = json!(policy);
        let result = s.dispatch("export.local_run", &params).expect("entry failure");
        assert_eq!(result["entries"][0]["state"], "failed", "{policy}");
        assert!(result["entries"][0]["reason"].as_str().expect("reason").contains("symbolic link"), "{policy}");
        assert!(!private.exists(), "the dangling link must never be followed under {policy}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_is_not_a_folder_grant() {
    let dir = tempfile::tempdir().expect("folder");
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("grant")).expect("link");
    let error = ExportHost::authorize(&dir.path().join("grant")).err().expect("refused link");
    assert!(error.message.contains("symbolic link"));
}

#[cfg(unix)]
#[test]
fn a_symlinked_directory_component_inside_the_grant_is_refused() {
    let dir = tempfile::tempdir().expect("folder");
    let outside = tempfile::tempdir().expect("outside");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("shots")).expect("link");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let err = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), &dir.path().join("shots"))).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::InvalidInput | ErrorKind::Unsupported));
    assert!(files_in(outside.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn even_an_in_root_symlinked_directory_component_is_refused() {
    let dir = tempfile::tempdir().expect("folder");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("real");
    std::os::unix::fs::symlink(&real, dir.path().join("alias")).expect("alias");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let err = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), &dir.path().join("alias"))).unwrap_err();
    assert!(err.message.contains("symbolic link"), "{err:?}");
    assert!(files_in(&real).is_empty());
}

#[cfg(unix)]
#[test]
fn a_directory_swapped_after_authorization_is_not_followed() {
    let dir = tempfile::tempdir().expect("grant");
    let outside = tempfile::tempdir().expect("outside");
    let sub = dir.path().join("shots");
    std::fs::create_dir(&sub).expect("subfolder");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let params = request(sources(&[json!({"node": id(1), "port": "out"})]), &sub);
    s.dispatch("export.local_plan", &params).expect("initial path is safe");
    std::fs::rename(&sub, dir.path().join("detached")).expect("rename");
    std::os::unix::fs::symlink(outside.path(), &sub).expect("swap to outside");
    let err = s.dispatch("export.local_run", &params).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::InvalidInput | ErrorKind::Unsupported));
    assert!(files_in(outside.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn a_parent_swapped_after_copy_is_refused_before_publication() {
    let dir = tempfile::tempdir().expect("grant");
    let outside = tempfile::tempdir().expect("outside");
    let sub = dir.path().join("shots");
    std::fs::create_dir(&sub).expect("subfolder");
    let s = session();
    let host = ExportHost::authorize(dir.path()).expect("authorize");
    let destination = host.pinned_destination(&sub).expect("pinned");
    let asset = match media(HERO) {
        Value::Media(asset) => asset,
        _ => panic!("media fixture"),
    };
    let result = destination.write_with_hook(s.assets(), &asset, "hero.png", true, || {
        std::fs::rename(&sub, dir.path().join("detached")).expect("rename parent");
        std::os::unix::fs::symlink(outside.path(), &sub).expect("replace with outside link");
    });
    let reason = result.unwrap_err().message;
    assert!(reason.contains("folder changed") || reason.contains("symbolic link"), "{reason}");
    assert!(files_in(outside.path()).is_empty(), "no write may follow the planted link");
}

#[cfg(unix)]
#[test]
fn a_file_link_swapped_in_after_copy_is_refused_before_overwrite() {
    let dir = tempfile::tempdir().expect("grant");
    let outside = tempfile::tempdir().expect("outside");
    let private = outside.path().join("private.png");
    std::fs::write(&private, b"untouched").expect("private file");
    let s = session();
    let host = ExportHost::authorize(dir.path()).expect("authorize");
    let destination = host.pinned_destination(dir.path()).expect("pinned");
    let asset = match media(HERO) {
        Value::Media(asset) => asset,
        _ => panic!("media fixture"),
    };
    let result = destination.write_with_hook(s.assets(), &asset, "hero.png", true, || {
        std::os::unix::fs::symlink(&private, dir.path().join("hero.png")).expect("plant link");
    });
    assert!(result.unwrap_err().message.contains("symbolic link"));
    assert_eq!(std::fs::read(&private).expect("private"), b"untouched");
}

#[test]
fn a_bad_request_is_refused_before_anything_is_resolved_or_written() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let node = json!({"node": id(1), "port": "out"});
    let cases: Vec<(&str, Json, ErrorKind)> = vec![
        (
            "an unknown parameter",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "chmod": "0777"}),
            ErrorKind::InvalidInput,
        ),
        ("no sources", json!({"canvas": C, "sources": [], "destination": dir.path().display().to_string()}), ErrorKind::InvalidInput),
        (
            "too many sources",
            json!({"canvas": C, "sources": vec![node.clone(); MAX_SOURCES + 1], "destination": dir.path().display().to_string()}),
            ErrorKind::InvalidInput,
        ),
        (
            "path traversal in a template",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "template": "../{title}"}),
            ErrorKind::InvalidInput,
        ),
        (
            "a separator in a template",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "template": "{title}\\\\{output}"}),
            ErrorKind::InvalidInput,
        ),
        (
            "an overlong destination",
            json!({"canvas": C, "sources": [node.clone()], "destination": format!("/{}", "a".repeat(nodecraft_format::export::MAX_DESTINATION_BYTES))}),
            ErrorKind::InvalidInput,
        ),
        (
            "a source missing its port",
            json!({"canvas": C, "sources": [{"node": id(1)}], "destination": dir.path().display().to_string()}),
            ErrorKind::InvalidInput,
        ),
        (
            "a source naming someone else's canvas",
            json!({"canvas": CanvasId::from_bytes([0xee; 16]), "sources": [node.clone()], "destination": dir.path().display().to_string()}),
            ErrorKind::NotFound,
        ),
        (
            "an unknown collision policy",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "on_collision": "silently_replace"}),
            ErrorKind::InvalidInput,
        ),
        (
            "an unknown token",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "template": "{prompt}"}),
            ErrorKind::InvalidInput,
        ),
        (
            "a template over the documented bound",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "template": format!("{{title}}{}", "x".repeat(200))}),
            ErrorKind::InvalidInput,
        ),
        (
            "a conversion this build does not have",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "format": {"mode": "convert", "mime": "image/jpeg"}}),
            ErrorKind::Unsupported,
        ),
        (
            "vectorisation",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().display().to_string(), "format": {"mode": "vectorise", "mime": "image/svg+xml"}}),
            ErrorKind::Unsupported,
        ),
        (
            "a folder that does not exist",
            json!({"canvas": C, "sources": [node.clone()], "destination": dir.path().join("gone").display().to_string()}),
            ErrorKind::InvalidInput,
        ),
    ];
    for (what, params, kind) in cases {
        for id in ["export.local_plan", "export.local_run"] {
            // Non-Unix runs stop at the unsupported secure writer before looking up a canvas;
            // the read-only plan still validates the bad canvas on every platform.
            let expected =
                if !cfg!(unix) && id == "export.local_run" && what == "a source naming someone else's canvas" { ErrorKind::Unsupported } else { kind };
            let error = s.enabled(id, &params).unwrap_err();
            assert_eq!(error.kind, expected, "{what} via {id} enabled");
            let error = s.dispatch(id, &params).unwrap_err();
            assert_eq!(error.kind, expected, "{what} via {id}");
            assert!(!error.message.is_empty(), "{what} has no reason to act on");
        }
    }
    assert_eq!(files_in(dir.path()), Vec::<String>::new(), "a refused request writes nothing");
}

#[test]
fn a_conversion_is_refused_with_the_type_and_the_alternative_not_an_approximation() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let error = s
        .dispatch(
            "export.local_run",
            &json!({
                "canvas": C,
                "sources": [json!({"node": id(1), "port": "out"})],
                "destination": dir.path().display().to_string(),
                "format": {"mode": "convert", "mime": "image/jpeg"}
            }),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("image/jpeg"), "{error:?}");
    assert!(error.message.contains("not implemented"), "{error:?}");
    assert_eq!(files_in(dir.path()), Vec::<String>::new(), "the stored bytes are never passed off as a conversion");
}

#[cfg(unix)]
#[test]
fn a_source_that_cannot_be_written_fails_only_its_own_entry() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let params = request(
        sources(&[
            json!({"node": id(1), "port": "out"}),
            json!({"node": NodeId::from_bytes([0xdd; 16]), "port": "out"}),
            json!({"node": id(3), "port": "out"}),
            json!({"node": id(1), "port": "nope"}),
            json!({"node": id(1), "port": "out", "item": 4}),
            json!({"node": id(1), "port": "out", "item": 100_000}),
        ]),
        dir.path(),
    );

    let preview = s.dispatch("export.local_plan", &params).expect("a preview of a partly impossible request");
    let why = reasons_of(&preview);
    assert_eq!(
        why,
        vec![
            reasons::UNKNOWN_NODE,
            reasons::NO_RESULT,
            // From here on these are `history.export`'s own words: the batch shares its resolver,
            // so it cannot describe a source any differently from the one-file command.
            "the node has no such output",
            "that output is a single value, not a list",
            reasons::ITEM_OUT_OF_RANGE,
        ],
        "{why:?}"
    );

    let run = s.dispatch("export.local_run", &params).expect("the good entry still runs");
    assert_eq!(run["counts"]["written"], 1, "one bad entry does not stop the export");
    assert_eq!(run["counts"]["failed"], 5);
    assert_eq!(run["entries"].as_array().expect("entries").len(), 6, "every request is answered");
    assert_eq!(run["partial"], true);
    assert_eq!(run["complete"], false);
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned()]);
    assert_eq!(run["entries"][1]["reason"], reasons::UNKNOWN_NODE);
    assert_eq!(run["entries"][2]["reason"], reasons::NO_RESULT);
}

#[cfg(unix)]
#[test]
fn a_text_result_is_refused_the_way_557_refuses_it() {
    // `history.export` says text does not export to a file and copying is the way out (#557). A
    // batch that quietly wrote `.txt` would contradict the shipped decision, so it refuses too —
    // per entry, so one text result never stops the images beside it.
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let run = s
        .dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"}), json!({"node": id(4), "port": "out"})]), dir.path()))
        .expect("the image still exports");
    assert_eq!(run["counts"]["written"], 1);
    assert_eq!(run["counts"]["failed"], 1);
    let reason = run["entries"][1]["reason"].as_str().expect("reason");
    assert_eq!(reason, reasons::UNSUPPORTED_VALUE, "{reason}");
    assert!(reason.contains("copy text instead"), "the refusal repeats #557's advice: {reason}");
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned()], "no .txt appears");
}

#[test]
fn a_list_output_needs_an_item_because_the_batch_uses_the_shared_resolver() {
    // #557 made (port, item) mean one thing everywhere: a list with no item is ambiguous and an
    // item on a single value is a mistake. The batch inherits both rather than inventing a
    // friendlier-but-different rule of its own.
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session_with(vec![ran_list(1, "takes", HERO, WIDE)], &[HERO, WIDE]);
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));

    let ambiguous = s.dispatch("export.local_plan", &request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path())).expect("preview");
    assert_eq!(ambiguous["entries"][0]["state"], "unavailable");
    assert_eq!(ambiguous["entries"][0]["reason"], "that output is a list: choose an item");
    let chosen_params = json!({"canvas": C, "sources": [json!({"node": id(1), "port": "out", "item": 1})],
        "destination": dir.path().display().to_string(), "template": "{title}-{output}-{index}-{item}"});
    let chosen_plan = s.dispatch("export.local_plan", &chosen_params).expect("chosen item preview");
    assert_eq!(chosen_plan["entries"][0]["file"], "takes-out-1-1.png");
    assert!(files_in(dir.path()).is_empty());

    #[cfg(unix)]
    let chosen = s.dispatch("export.local_run", &chosen_params).expect("run");
    #[cfg(unix)]
    {
        assert_eq!(chosen["counts"]["written"], 1);
        assert_eq!(chosen["entries"][0]["file"], "takes-out-1-1.png", "the {{item}} token is filled and the name is unique");
        assert_eq!(std::fs::read(dir.path().join("takes-out-1-1.png")).expect("read"), WIDE);
    }
    #[cfg(not(unix))]
    assert!(files_in(dir.path()).is_empty(), "a rejected source in a plan writes nothing");
}

#[cfg(unix)]
#[test]
fn a_result_whose_bytes_are_gone_fails_that_entry_and_writes_nothing_for_it() {
    // The store is checked while planning, so a result the project no longer holds is reported
    // rather than half-written — the same "verified before the file appears" promise #557 makes.
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session_with(vec![ran(1, "hero", HERO)], &[WIDE]);
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let run = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path())).expect("run");
    assert_eq!(run["counts"]["failed"], 1);
    assert_eq!(run["entries"][0]["reason"], reasons::READING_ASSET);
    assert_eq!(files_in(dir.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_reason_never_repeats_the_folder_a_file_name_or_a_node_title() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    // A folder the person can point at, and a result the project does not hold, so entries fail.
    let run = s
        .dispatch(
            "export.local_run",
            &request(
                sources(&[json!({"node": id(1), "port": "out"}), json!({"node": id(3), "port": "out"}), json!({"node": id(4), "port": "out"})]),
                dir.path(),
            ),
        )
        .expect("run");
    let folder = dir.path().to_string_lossy().into_owned();
    for entry in run["entries"].as_array().expect("entries") {
        let Some(reason) = entry["reason"].as_str() else { continue };
        assert!(!reason.contains(&folder), "a reason carried the destination: {reason}");
        assert!(!reason.contains("hero") && !reason.contains("words"), "a reason carried a stored name: {reason}");
    }
}

#[cfg(unix)]
#[test]
fn the_collision_policy_is_honoured_and_reported_per_entry() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());

    s.dispatch("export.local_run", &params).expect("first");
    let skipped = s.dispatch("export.local_run", &params).expect("second");
    assert_eq!(skipped["counts"]["skipped"], 1);
    assert_eq!(skipped["entries"][0]["reason"], reasons::SKIPPED);
    assert_eq!(skipped["partial"], true);
    assert_eq!(std::fs::read(dir.path().join("hero-out-1.png")).expect("read"), HERO, "skip never touches the existing file");

    let overwritten = s.dispatch(
        "export.local_run",
        &json!({"canvas": C, "sources": [json!({"node": id(1), "port": "out"})], "destination": dir.path().display().to_string(), "on_collision": "overwrite"}),
    )
    .expect("overwrite");
    assert_eq!(overwritten["counts"]["written"], 1);
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned()]);

    let numbered = s.dispatch(
        "export.local_run",
        &json!({"canvas": C, "sources": [json!({"node": id(1), "port": "out"})], "destination": dir.path().display().to_string(), "on_collision": "numbered"}),
    )
    .expect("numbered");
    assert_eq!(numbered["entries"][0]["file"], "hero-out-1 (2).png");
    assert_eq!(files_in(dir.path()), vec!["hero-out-1 (2).png".to_owned(), "hero-out-1.png".to_owned()]);
}

#[cfg(unix)]
#[test]
fn a_collision_with_a_file_the_person_wrote_is_reported_not_overwritten() {
    // The other half of #557's "never replace without consent", applied across a batch.
    let dir = tempfile::tempdir().expect("temp");
    std::fs::write(dir.path().join("hero-out-1.png"), b"mine").expect("seed");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let run = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path())).expect("run");
    assert_eq!(run["counts"]["skipped"], 1);
    assert_eq!(std::fs::read(dir.path().join("hero-out-1.png")).expect("read"), b"mine");
}

#[cfg(unix)]
#[test]
fn a_cancel_stops_the_run_and_reports_every_entry_it_never_started() {
    let dir = tempfile::tempdir().expect("temp");
    let host = ExportHost::authorize(dir.path()).expect("authorize");
    let mut s = session();
    s.swap_resource(Some(host.clone()));
    // A person presses Cancel before the run: nothing is written at all, and the report says so
    // rather than coming back empty.
    host.cancel().cancel();
    let run = s
        .dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"}), json!({"node": id(2), "port": "out"})]), dir.path()))
        .expect("a cancelled run is a result, not an error");
    assert_eq!(run["cancelled"], true);
    assert_eq!(run["complete"], false);
    assert_eq!(run["counts"]["not_started"], 2);
    assert_eq!(run["counts"]["written"], 0);
    assert_eq!(reasons_of(&run), vec![reasons::NOT_STARTED, reasons::NOT_STARTED]);
    assert_eq!(files_in(dir.path()), Vec::<String>::new(), "a cancel leaves nothing behind");
}

#[test]
fn a_hostile_template_is_refused_before_writing_any_file() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let error = s
        .dispatch(
            "export.local_run",
            &json!({
                "canvas": C,
                "sources": [json!({"node": id(1), "port": "out"})],
                "destination": dir.path().display().to_string(),
                "template": "../../escape/{title}"
            }),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("path separators"));
    assert_eq!(files_in(dir.path()), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_programmatic_call_needs_no_dialog_and_no_host_service() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session();
    // No service, no run host, no picker: only the folder the app authorized. A palette, CLI,
    // control channel or agent call takes exactly this shape.
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    assert_eq!(s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path())).expect("run")["counts"]["written"], 1);
    assert!(!s.can_undo(), "an export left nothing to undo");
}

#[cfg(unix)]
#[test]
fn batch_and_single_file_export_share_verified_copy_without_sharing_path_publication() {
    // The two commands are different asks and must not interfere: one file the person pointed at,
    // one folder the app authorized. Both must work on the same session.
    let dir = tempfile::tempdir().expect("temp");
    let chosen = dir.path().join("picked.png");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));

    let one = s.dispatch("history.export", &json!({"canvas": C, "id": id(1), "generation": GenerationId::from_bytes([1; 16]), "path": chosen})).expect("one");
    assert_eq!(one, json!({"bytes": HERO.len()}), "history.export's own result shape is unchanged");
    let batch = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path())).expect("batch");
    assert_eq!(batch["counts"]["written"], 1);
    assert_eq!(files_in(dir.path()), vec!["hero-out-1.png".to_owned(), "picked.png".to_owned()]);
    assert_eq!(std::fs::read(&chosen).expect("read"), HERO);
}

fn named(stem: &str) -> nodecraft_format::export::ExportName {
    nodecraft_format::export::ExportName { stem: stem.to_owned(), extension: Some("png".to_owned()) }
}

#[test]
fn a_name_repeated_in_one_batch_follows_the_policy_and_never_replaces_its_own_file() {
    let (first, second) = (named("hero"), named("hero"));
    let batch = [&first, &second];
    let free = |_: &str| -> Result<bool, String> { Ok(false) };
    assert_eq!(
        super::assign_names(&batch, Collision::Overwrite, &free),
        vec![Ok(Some("hero.png".to_owned())), Err(reasons::DUPLICATE_NAME.to_owned())],
        "overwrite refuses the repeat rather than replacing the first result"
    );
    assert_eq!(super::assign_names(&batch, Collision::Skip, &free), vec![Ok(Some("hero.png".to_owned())), Ok(None)]);
    assert_eq!(super::assign_names(&batch, Collision::Numbered, &free), vec![Ok(Some("hero.png".to_owned())), Ok(Some(first.with_suffix(2)))]);
}

#[test]
fn names_that_differ_only_in_case_are_one_file_on_case_insensitive_folders() {
    let (upper, lower) = (nodecraft_format::export::ExportName { stem: "Hero".to_owned(), extension: Some("png".to_owned()) }, named("hero"));
    let free = |_: &str| -> Result<bool, String> { Ok(false) };
    assert_eq!(
        super::assign_names(&[&upper, &lower], Collision::Overwrite, &free),
        vec![Ok(Some("Hero.png".to_owned())), Err(reasons::DUPLICATE_NAME.to_owned())],
        "macOS and Windows would replace Hero.png with hero.png, so the repeat is refused"
    );
    assert_eq!(super::assign_names(&[&upper, &lower], Collision::Skip, &free), vec![Ok(Some("Hero.png".to_owned())), Ok(None)]);
}

#[test]
fn numbered_reports_when_no_suffix_is_free() {
    let only = named("hero");
    let everything_taken = |_: &str| -> Result<bool, String> { Ok(true) };
    assert_eq!(super::assign_names(&[&only], Collision::Numbered, &everything_taken), vec![Err(reasons::NO_FREE_NAME.to_owned())]);
}

#[cfg(unix)]
#[test]
fn two_results_with_one_name_never_replace_each_other_in_a_run() {
    let dir = tempfile::tempdir().expect("temp");
    let mut s = session_with(vec![ran(1, "hero", HERO), ran(2, "hero", WIDE)], &[HERO, WIDE]);
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let mut params = request(sources(&[json!({"node": id(1), "port": "out"}), json!({"node": id(2), "port": "out"})]), dir.path());
    params["template"] = json!("{title}");
    params["on_collision"] = json!("overwrite");

    let preview = s.dispatch("export.local_plan", &params).expect("preview");
    let run = s.dispatch("export.local_run", &params).expect("run");
    assert_eq!(run["counts"]["written"], 1);
    assert_eq!(run["counts"]["failed"], 1);
    assert_eq!(names(&run), names(&preview), "the preview names the same files the run writes");
    assert_eq!(std::fs::read(dir.path().join("hero.png")).expect("read"), HERO, "the first result survives the repeat");
    assert!(reasons_of(&run).contains(&reasons::DUPLICATE_NAME.to_owned()));
}

#[cfg(unix)]
#[test]
fn a_cancel_stops_one_run_and_not_the_run_after_it() {
    let dir = tempfile::tempdir().expect("temp");
    let host = ExportHost::authorize(dir.path()).expect("authorize");
    let cancel = host.cancel().clone();
    let mut s = session();
    s.swap_resource(Some(host));
    let params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());

    cancel.cancel();
    let stopped = s.dispatch("export.local_run", &params).expect("stopped run");
    assert_eq!(stopped["cancelled"], true);
    assert_eq!(stopped["counts"]["not_started"], 1);

    let next = s.dispatch("export.local_run", &params).expect("next run");
    assert_eq!(next["cancelled"], false, "the cancel did not outlive the run it stopped");
    assert_eq!(next["counts"]["written"], 1);
}

#[cfg(unix)]
#[test]
fn an_export_file_gets_the_umask_mode_and_an_overwrite_keeps_the_existing_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("temp");
    let control = dir.path().join("control");
    std::fs::write(&control, b"x").expect("control");
    let umask_mode = std::fs::metadata(&control).expect("metadata").permissions().mode() & 0o777;
    let target = dir.path().join("hero-out-1.png");
    let mode = |path: &std::path::Path| std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;

    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(dir.path()).expect("authorize")));
    let mut params = request(sources(&[json!({"node": id(1), "port": "out"})]), dir.path());
    s.dispatch("export.local_run", &params).expect("first run");
    assert_eq!(mode(&target), umask_mode, "a new export file gets the same permissions a plain file gets");

    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).expect("chmod");
    params["on_collision"] = json!("overwrite");
    s.dispatch("export.local_run", &params).expect("overwrite");
    assert_eq!(mode(&target), 0o640, "an overwrite keeps the mode of the file it replaces");
}

#[test]
fn an_app_authorizes_at_most_sixteen_folders() {
    let dir = tempfile::tempdir().expect("temp");
    let roots = vec![dir.path().to_path_buf(); MAX_ROOTS + 1];
    let error = ExportHost::new(roots, nodecraft_format::asset_source::TransferCancel::default()).err().expect("refused");
    assert_eq!(error.kind, ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn a_grant_root_replaced_after_authorization_writes_nothing_into_the_replacement() {
    let parent = tempfile::tempdir().expect("temp");
    let root = parent.path().join("grant");
    std::fs::create_dir(&root).expect("grant");
    let mut s = session();
    s.swap_resource(Some(ExportHost::authorize(&root).expect("authorize")));
    // The person's folder is moved away and a new folder takes its name: the grant must not follow
    // the name to the impostor.
    std::fs::rename(&root, parent.path().join("moved")).expect("move");
    std::fs::create_dir(&root).expect("replacement");

    let run = s.dispatch("export.local_run", &request(sources(&[json!({"node": id(1), "port": "out"})]), &root)).expect("run");
    assert_eq!(run["counts"]["written"], 0);
    assert_eq!(run["counts"]["failed"], 1);
    assert_eq!(files_in(&root), Vec::<String>::new(), "the replacement folder receives nothing");
}
