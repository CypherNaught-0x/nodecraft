use super::Clip;
use crate::Session;
use nodecraft_graph::{Canvas, Change, Generation, Node, Port, Project};
use nodecraft_types::{AssetRef, Blake3, CanvasId, ErrorKind, GenerationId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const N: NodeId = NodeId::from_bytes([2; 16]);
const IMAGE: GenerationId = GenerationId::from_bytes([3; 16]);
const TEXT: GenerationId = GenerationId::from_bytes([4; 16]);
const VIDEO: GenerationId = GenerationId::from_bytes([5; 16]);
const BROKEN: GenerationId = GenerationId::from_bytes([6; 16]);
const WIDE: GenerationId = GenerationId::from_bytes([8; 16]);

/// A PNG one pixel wider than a copy takes.
fn wide() -> Vec<u8> {
    let width = super::MAX_SIDE + 1;
    nodecraft_media::encode_png(width, 1, &vec![0; width as usize * 4], &nodecraft_media::Limits::default()).unwrap()
}

/// A 2×1 PNG: one red pixel, one half-transparent blue one.
fn png() -> Vec<u8> {
    nodecraft_media::encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128], &nodecraft_media::Limits::default()).unwrap()
}

fn asset(bytes: &[u8], mime: &str) -> AssetRef {
    AssetRef { hash: Blake3::from_bytes(*blake3::hash(bytes).as_bytes()), mime: mime.parse().unwrap(), meta: Default::default() }
}

/// A node with an image, a text, an unreadable and an oversized image result, and one with a
/// video; `clipboard` collects what is copied.
fn session(clipboard: Option<Rc<RefCell<Vec<Clip>>>>) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Copy").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = Node::new(N, "test.gen");
    node.outputs = vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }];
    node.history.push(Generation::new(IMAGE, vec![Value::Media(asset(&png(), "image/png"))]));
    node.history.push(Generation::new(TEXT, vec![Value::Text("a quiet forest".into())]));
    node.history.push(Generation::new(BROKEN, vec![Value::Media(asset(b"not a picture", "image/png"))]));
    node.history.push(Generation::new(WIDE, vec![Value::Media(asset(&wide(), "image/png"))]));
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut clip = Node::new(NodeId::from_bytes([7; 16]), "test.gen");
    clip.outputs = vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Video, many: false } }];
    clip.history.push(Generation::new(VIDEO, vec![Value::Media(asset(b"moov", "video/mp4"))]));
    Change::AddNode { canvas: C, node: clip }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    for bytes in [png(), b"not a picture".to_vec(), b"moov".to_vec(), wide()] {
        session.import_asset(&bytes).unwrap();
    }
    if let Some(copied) = clipboard {
        session
            .register_clipboard_host(move |clip| {
                copied.borrow_mut().push(clip);
                Ok(())
            })
            .unwrap();
    }
    session
}

fn copy(id: NodeId, generation: GenerationId) -> serde_json::Value {
    json!({"canvas": C, "id": id, "generation": generation})
}

#[test]
fn text_is_copied_as_text_and_an_image_as_its_pixels() {
    let copied = Rc::new(RefCell::new(Vec::new()));
    let mut s = session(Some(Rc::clone(&copied)));
    let revision = s.revision();
    assert_eq!(s.dispatch("history.copy", &copy(N, TEXT)).unwrap(), json!({"copied": "text", "chars": 14}));
    assert_eq!(s.dispatch("history.copy", &copy(N, IMAGE)).unwrap(), json!({"copied": "image", "width": 2, "height": 1}));
    assert_eq!(*copied.borrow(), vec![Clip::Text("a quiet forest".into()), Clip::Image { width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 128] }]);
    assert_eq!(s.revision(), revision, "copying doesn't edit the document");
    assert!(!s.can_undo());
}

#[test]
fn other_media_and_unreadable_images_are_refused_before_the_clipboard() {
    let copied = Rc::new(RefCell::new(Vec::new()));
    let mut s = session(Some(Rc::clone(&copied)));
    let video = s.dispatch("history.copy", &copy(NodeId::from_bytes([7; 16]), VIDEO)).unwrap_err();
    assert_eq!(video.kind, ErrorKind::Unsupported);
    assert!(video.message.contains("export"), "{}", video.message);
    assert!(s.enabled("history.copy", &copy(NodeId::from_bytes([7; 16]), VIDEO)).is_err());
    assert_eq!(s.dispatch("history.copy", &copy(N, BROKEN)).unwrap_err().kind, ErrorKind::InvalidInput);
    let missing = s.dispatch("history.copy", &copy(N, GenerationId::from_bytes([9; 16]))).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::NotFound);
    assert!(copied.borrow().is_empty());
}

#[test]
fn a_host_without_a_clipboard_refuses_and_says_so() {
    let mut s = session(None);
    assert_eq!(s.enabled("history.copy", &copy(N, TEXT)).unwrap_err().kind, ErrorKind::Unsupported);
    let refused = s.dispatch("history.copy", &copy(N, TEXT)).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert!(refused.message.contains("clipboard"), "{}", refused.message);
}

#[test]
fn a_clipboard_that_fails_reports_its_error_and_a_second_host_is_refused() {
    let mut s = session(None);
    s.register_clipboard_host(|_| Err(nodecraft_types::Error::new(ErrorKind::Busy, "the clipboard is busy"))).unwrap();
    assert_eq!(s.register_clipboard_host(|_| Ok(())).unwrap_err().kind, ErrorKind::Conflict);
    let failed = s.dispatch("history.copy", &copy(N, TEXT)).unwrap_err();
    assert_eq!((failed.kind, failed.message.as_str()), (ErrorKind::Busy, "the clipboard is busy"));
}

#[test]
fn an_image_over_the_side_cap_is_refused_without_decoding_it() {
    let copied = Rc::new(RefCell::new(Vec::new()));
    let mut s = session(Some(Rc::clone(&copied)));
    let refused = s.dispatch("history.copy", &copy(N, WIDE)).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert!(refused.message.contains("8192"), "{}", refused.message);
    assert!(copied.borrow().is_empty());
}
