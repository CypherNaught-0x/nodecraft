use super::*;
use nodecraft_graph::{Canvas, Change, Node, Project};
use nodecraft_types::{EdgeId, ProjectId};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const FOLDER: NodeId = NodeId::from_bytes([9; 16]);
const AT: Position = Position { x: 10.0, y: 20.0 };
const NEW: FolderTarget = FolderTarget::New { canvas: C, at: AT, id: FOLDER };
const AGAIN: FolderTarget = FolderTarget::Existing { canvas: C, id: FOLDER };

fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Folder").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Session::new(project).unwrap()
}

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(3, 2, &[shade; 3 * 2 * 4], &Limits::default()).unwrap()
}

fn card(s: &Session) -> &Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&FOLDER).unwrap()
}

fn names(s: &Session) -> Vec<Value> {
    folder::values(card(s), folder::ALL_NAMES).unwrap()
}

fn texts(values: &[&str]) -> Vec<Value> {
    values.iter().map(|v| Value::Text((*v).into())).collect()
}

#[test]
fn a_folder_becomes_one_card_of_its_main_kind_in_one_undo_step() {
    let mut s = session();
    let before = s.document().clone();
    let (a, b) = (png(10), png(200));
    let files = [
        Incoming { name: "shot10.png", bytes: &a },
        Incoming { name: "notes.txt", bytes: b"a note" },
        Incoming { name: "raw/shot2.png", bytes: &b },
        Incoming { name: "blob.bin", bytes: &[0, 159, 146, 150] },
        // The same picture twice is stored once and held twice.
        Incoming { name: "copy.png", bytes: &a },
    ];
    let imported = import_folder(&mut s, NEW, "Shoot", &files).unwrap();
    assert_eq!((imported.node, imported.modality, imported.files), (FOLDER, Modality::Image, 3));
    assert_eq!(imported.skipped.len(), 2);
    assert_eq!(imported.skipped.first().unwrap(), &("notes.txt".to_owned(), "the folder holds images".to_owned()));
    assert!(imported.notice().unwrap().contains("notes.txt"));
    let node = card(&s);
    assert_eq!((node.kind.as_str(), node.pos.x, node.pos.y, folder::folder_name(node)), (folder::ID, 10.0, 20.0, "Shoot"));
    assert_eq!(node.outputs.first().map(|p| p.port_type.modality), Some(Modality::Image));
    assert_eq!(names(&s), texts(&["copy.png", "shot2.png", "shot10.png"]));
    let Some(Value::Media(asset)) = folder::values(node, folder::FILE).unwrap().first().cloned() else { panic!("an image") };
    assert_eq!((asset.mime.essence(), asset.meta.width), ("image/png", Some(3)));
    assert!(s.assets().contains(&asset.hash));
    assert_eq!(s.assets().len(), 2);
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(s.document(), &before);
}

#[test]
fn a_text_folder_holds_its_text_and_nothing_usable_adds_nothing() {
    let mut s = session();
    let image = png(1);
    let files = [Incoming { name: "b.txt", bytes: b"second" }, Incoming { name: "a.txt", bytes: b"first" }, Incoming { name: "c.png", bytes: &image }];
    let imported = import_folder(&mut s, NEW, "Prompts", &files).unwrap();
    assert_eq!((imported.modality, imported.files), (Modality::Text, 2));
    assert_eq!(folder::values(card(&s), folder::ALL_FILES).unwrap(), texts(&["first", "second"]));

    let mut s = session();
    let before = s.document().clone();
    let junk = [Incoming { name: "empty", bytes: b"" }, Incoming { name: "blob", bytes: &[0, 159, 146, 150] }];
    let error = import_folder(&mut s, NEW, "Junk", &junk).unwrap_err();
    assert!(error.message.contains("nothing in Junk can be added"), "{}", error.message);
    assert!(import_folder(&mut s, NEW, "Empty", &[]).is_err());
    assert_eq!(s.document(), &before);
    assert_eq!(s.assets().len(), 0);
}

#[test]
fn importing_again_refills_the_card_and_keeps_its_settings_and_wires() {
    let mut s = session();
    let (a, b, c) = (png(1), png(2), png(3));
    import_folder(&mut s, NEW, "Shoot", &[Incoming { name: "a.png", bytes: &a }]).unwrap();
    s.dispatch("node.param_set", &serde_json::json!({"canvas": C, "id": FOLDER, "name": "reverse", "value": {"type": "bool", "value": true}})).unwrap();
    // A router taking the folder's images.
    let router = NodeId::from_bytes([7; 16]);
    let node = s
        .kinds()
        .get(nodecraft_nodes::router::ID)
        .unwrap()
        .instantiate_instance(router, nodecraft_nodes::router::config(Modality::Image), &Default::default())
        .unwrap();
    s.dispatch("canvas.paste", &serde_json::json!({"canvas": C, "nodes": [node], "edges": []})).unwrap();
    let edge =
        serde_json::json!({"id": EdgeId::from_bytes([5; 16]), "from": {"node": FOLDER, "port": "file"}, "to": {"node": router, "port": "in"}, "order": 0});
    s.dispatch("edge.connect", &serde_json::json!({"canvas": C, "edge": edge})).unwrap();
    let before = s.document().clone();

    // Wired, it stays an image folder: the text is left out.
    let files = [
        Incoming { name: "b.png", bytes: &b },
        Incoming { name: "c.png", bytes: &c },
        Incoming { name: "x.txt", bytes: b"x" },
        Incoming { name: "y.txt", bytes: b"y" },
    ];
    let imported = import_folder(&mut s, AGAIN, "Shoot 2", &files).unwrap();
    assert_eq!((imported.modality, imported.files, imported.skipped.len()), (Modality::Image, 2, 2));
    assert_eq!(names(&s), texts(&["c.png", "b.png"]), "its reverse order stays");
    assert_eq!(folder::folder_name(card(&s)), "Shoot 2");
    assert_eq!(s.document().canvases().get(&C).unwrap().edges().len(), 1);
    s.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(s.document(), &before);

    // Unwired, it takes the kind most of its files are.
    s.dispatch("canvas.delete", &serde_json::json!({"canvas": C, "nodes": [router], "edges": []})).unwrap();
    let words = [Incoming { name: "x.txt", bytes: b"x" }, Incoming { name: "y.txt", bytes: b"y" }, Incoming { name: "b.png", bytes: &b }];
    assert_eq!(import_folder(&mut s, AGAIN, "Words", &words).unwrap().modality, Modality::Text);
    assert_eq!(card(&s).outputs.first().map(|p| p.port_type.modality), Some(Modality::Text));
    let other = NodeId::from_bytes([1; 16]);
    assert!(import_folder(&mut s, FolderTarget::Existing { canvas: C, id: other }, "Words", &words).is_err());
}

#[test]
fn a_folder_too_large_for_one_command_leaves_files_out_instead_of_failing() {
    let mut s = session();
    let names: Vec<String> = (0..folder::MAX_FILES).map(|n| format!("{n:04}{}.txt", "n".repeat(folder::MAX_NAME_BYTES - 8))).collect();
    let files: Vec<Incoming<'_>> = names.iter().map(|name| Incoming { name, bytes: b"words" }).collect();
    let imported = import_folder(&mut s, NEW, "Many", &files).unwrap();
    assert!(imported.files > 0 && imported.files < folder::MAX_FILES, "{}", imported.files);
    assert_eq!(imported.files + imported.skipped.len(), folder::MAX_FILES);
    assert!(imported.notice().unwrap().contains("names and text"));
}
