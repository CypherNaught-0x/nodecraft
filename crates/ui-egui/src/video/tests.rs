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
    let kind = kinds.get("source.video").unwrap();
    let configured = kind.resolve_instance(&video).unwrap();
    let unconfigured = kind.resolve_instance(&empty).unwrap();
    let grown = crate::card::world_size(&video, Some(&configured), &t).y - crate::card::world_size(&empty, Some(&unconfigured), &t).y;
    // A configured asset adds its real Remove button row as well as the media region.
    let remove_row = t.body_size + t.control_gap * 2.0;
    assert_eq!(remove_row, 25.0);
    assert_eq!(grown, t.card_preview_height + remove_row);
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

#[cfg(not(target_arch = "wasm32"))]
mod native_rules {
    use super::super::native::{Retry, stale, transient, wanted_us};
    use nodecraft_media::video::VideoError;

    const STEP: u64 = 40_000;

    #[test]
    fn frames_are_asked_for_at_their_start_and_never_past_the_last() {
        assert_eq!(wanted_us(0, 2_000_000, STEP, None), Some(0));
        assert_eq!(wanted_us(1_039_999, 2_000_000, STEP, None), Some(1_000_000));
        // At or past the end: the last frame, a step before it.
        assert_eq!(wanted_us(2_000_000, 2_000_000, STEP, None), Some(1_960_000));
        assert_eq!(wanted_us(u64::MAX, 2_000_000, STEP, None), Some(1_960_000));
        // A clip shorter than a frame has one, at the start.
        assert_eq!(wanted_us(500, 10, STEP, None), Some(0));
        assert_eq!(wanted_us(500, 0, 0, None), Some(0));
    }

    #[test]
    fn a_missing_frame_moves_the_end_so_it_isnt_asked_for_again() {
        // No frame at 1.96 s: the next ask is a frame earlier, never 1.96 s again.
        let end = Some(1_960_000);
        assert_eq!(wanted_us(2_000_000, 2_000_000, STEP, end), Some(1_920_000));
        assert!(wanted_us(u64::MAX, 2_000_000, STEP, end).is_some_and(|t| t < 1_960_000));
        // Earlier positions are untouched.
        assert_eq!(wanted_us(1_000_000, 2_000_000, STEP, end), Some(1_000_000));
        // Settled on the last frame shown (end = shown + a step): it is what's wanted, so
        // nothing new is asked for.
        assert_eq!(wanted_us(2_000_000, 2_000_000, STEP, Some(1_000_000 + STEP)), Some(1_000_000));
        // No frame at all: nothing to ask for.
        assert_eq!(wanted_us(0, 2_000_000, STEP, Some(0)), None);
    }

    #[test]
    fn work_that_failed_for_now_is_retried_a_few_times_then_given_up() {
        assert!(transient(VideoError::Limit) && transient(VideoError::Timeout) && transient(VideoError::Io));
        assert!(!transient(VideoError::Unsupported) && !transient(VideoError::InvalidMetadata) && !transient(VideoError::NoFrame));
        assert_eq!(Retry::after(None, VideoError::Unsupported, 0.0), None, "a format that isn't supported never will be");
        let mut retry = None;
        let mut tries = 0;
        while let Some(next) = Retry::after(retry, VideoError::Limit, 10.0) {
            assert!(!next.due(10.0) && next.due(100.0), "waits before trying again");
            retry = Some(next);
            tries += 1;
            assert!(tries < 100);
        }
        assert_eq!(tries, 4, "gives up after five tries");
    }

    #[test]
    fn only_what_was_off_screen_last_frame_is_evicted() {
        assert!(!stale(10, 10), "shown this frame");
        assert!(!stale(9, 10), "shown last frame, before this frame's cards are drawn");
        assert!(stale(8, 10));
        assert!(!stale(u64::MAX, 0), "the clock wrapping doesn't evict what's on screen");
    }
}
