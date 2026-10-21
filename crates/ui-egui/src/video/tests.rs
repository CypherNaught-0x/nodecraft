use super::*;
use nodecraft_graph::Generation;
use nodecraft_types::Blake3;
use nodecraft_types::{GenerationId, NodeId};

fn media(mime: &str) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: mime.parse().unwrap(), meta: Default::default() })
}

fn node(kind: &str, params: &[(&str, Value)]) -> Node {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let supplied: nodecraft_types::ParamValues = params.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    kinds.get(kind).unwrap().instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &supplied).unwrap()
}

#[test]
fn a_source_video_shows_its_asset_and_nothing_else_does_by_mistake() {
    let video = node("source.video", &[("asset", media("video/webm"))]);
    assert_eq!(video_of(&video).map(|a| a.mime.essence()), Some("video/webm"));
    let image = node("source.image", &[("asset", media("image/png"))]);
    assert!(video_of(&image).is_none());
    // A source.video holding something that isn't a video shows none.
    let mut wrong = Node::new(NodeId::from_bytes([2; 16]), "source.video");
    wrong.params.insert("asset".into(), media("image/png"));
    assert!(video_of(&wrong).is_none());
}

#[test]
fn a_result_shows_its_first_video_inside_lists_too() {
    let mut text = node("source.text", &[]);
    let id = GenerationId::from_bytes([3; 16]);
    text.history.push(Generation::new(id, vec![Value::Text("caption".into()), Value::List(vec![media("image/png"), media("video/mp4")])]));
    assert!(video_of(&text).is_none(), "only the active result counts");
    text.active = Some(id);
    assert_eq!(video_of(&text).map(|a| a.mime.essence()), Some("video/mp4"));
}

#[test]
fn a_video_card_has_room_for_its_player() {
    let t = *crate::theme::ThemeKind::Dark.tokens();
    let video = node("source.video", &[("asset", media("video/webm"))]);
    let empty = node("source.video", &[]);
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let d = kinds.get("source.video").ok().map(|k| k.descriptor());
    let grown = crate::card::world_size(&video, d, &t).y - crate::card::world_size(&empty, d, &t).y;
    assert_eq!(grown, t.card_preview_height);
}

#[test]
fn clocks_read_like_clocks() {
    assert_eq!(clock(0), "0:00");
    assert_eq!(clock(9_999_999), "0:09");
    assert_eq!(clock(61_000_000), "1:01");
    assert_eq!(clock(3_726_000_000), "1:02:06");
    assert_eq!(clock(u64::MAX), "5124095576:01:49");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn unset_tools_say_how_to_set_them_and_start_nothing() {
    let ctx = egui::Context::default();
    let mut videos = Videos::default();
    videos.configure(None);
    let asset = AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "video/webm".parse().unwrap(), meta: Default::default() };
    let view = videos.view(&ctx, NodeId::from_bytes([1; 16]), &asset, 256.0, || panic!("nothing is read without tools"));
    assert_eq!(view, VideoView::Unavailable(UNCONFIGURED));
    assert_eq!(videos.test_counts(), (0, 0, 0));
}
