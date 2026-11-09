//! Structural rules we expect Final Cut Pro and DaVinci Resolve to need when they import an FCPXML
//! (#1066), checked on the representative archive by reading it back as a document, not by
//! matching text. They are inferred from the FCPXML format, not taken from Apple's DTD, and some
//! are this writer's own invariants (noted where they are). This does not run either editor: `docs/timeline-interchange.md` ("Checking an import") says how
//! a person checks one, and what has been checked.
use super::*;

/// A rational FCPXML time (`"48/30s"`, `"2s"`, `"0s"`) as a numerator and denominator.
fn time(text: &str) -> Option<(i128, i128)> {
    let body = text.strip_suffix('s')?;
    let (n, d) = match body.split_once('/') {
        Some((n, d)) => (n.parse().ok()?, d.parse().ok()?),
        None => (body.parse().ok()?, 1),
    };
    (d > 0 && n >= 0).then_some((n, d))
}

/// `a` = `b` for two rational times.
fn same(a: (i128, i128), b: (i128, i128)) -> bool {
    a.0 * b.1 == b.0 * a.1
}

fn add(a: (i128, i128), b: (i128, i128)) -> (i128, i128) {
    (a.0 * b.1 + b.0 * a.1, a.1 * b.1)
}

/// Whether `t` is a whole number of frames of `frame` (a frame's duration).
fn whole_frames(t: (i128, i128), frame: (i128, i128)) -> bool {
    // t / frame = (t.0 * frame.1) / (t.1 * frame.0) must be an integer.
    (t.0 * frame.1) % (t.1 * frame.0) == 0
}

const TIMES: [&str; 5] = ["offset", "start", "duration", "tcStart", "time"];
const CLIPS: [&str; 4] = ["asset-clip", "video", "audio", "title"];

fn attr(node: roxmltree::Node<'_, '_>, name: &str) -> (i128, i128) {
    time(node.attribute(name).unwrap_or_else(|| panic!("<{}> has no {name}", node.tag_name().name())))
        .unwrap_or_else(|| panic!("<{}> {name} isn't a rational time", node.tag_name().name()))
}

/// A `src` URL path as the file name it stands for: every `%XX` decoded as bytes, then UTF-8.
fn decoded(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match (byte, hex) {
            (b'%', Some(value)) => {
                out.push(value);
                i += 3;
            }
            _ => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("a src decodes to UTF-8")
}

/// Exports the representative cut and checks its document and media against the import rules.
#[test]
fn the_document_follows_the_rules_editors_need_on_import() {
    check(representative());
}

/// The same with a picture that plays on past the main track's end, which hangs from a trailing
/// gap on the spine.
#[test]
fn a_layer_past_the_main_track_hangs_from_a_trailing_gap() {
    // The main track ends at 1.6 s; the 0.4 s still starts at 1.5 s.
    let s = representative_with_still_at(1_500_000);
    let xml = check(s);
    assert!(xml.contains("<gap "), "a trailing gap carries it: {xml}");
}

/// Exports `s`'s cut, checks it, and returns the document.
fn check(mut s: Session) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("practice.zip");
    s.dispatch("timeline.export_interchange", &json!({"canvas":CANVAS,"id":EDITOR,"path":path})).unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().unwrap().into_owned()).collect();
    let mut xml = String::new();
    zip.by_name("Practice-cut.fcpxml").unwrap().read_to_string(&mut xml).unwrap();

    // Well-formed XML whose root both editors read: FCPXML 1.10.
    // `<!DOCTYPE fcpxml>`, which both editors expect, names no DTD to load.
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
    let document = roxmltree::Document::parse_with_options(&xml, options).expect("well-formed XML");
    let root = document.root_element();
    assert_eq!(root.tag_name().name(), "fcpxml");
    assert_eq!(root.attribute("version"), Some("1.10"));

    // Resources first, then library > event > project > sequence > spine, one of each.
    let children: Vec<&str> = root.children().filter(roxmltree::Node::is_element).map(|n| n.tag_name().name()).collect();
    assert_eq!(children, ["resources", "library"]);
    let path_of = |names: &[&str]| {
        let mut node = root;
        for name in names {
            let found: Vec<_> = node.children().filter(|n| n.has_tag_name(*name)).collect();
            assert_eq!(found.len(), 1, "one <{name}>");
            node = found[0];
        }
        node
    };
    let sequence = path_of(&["library", "event", "project", "sequence"]);
    let spine = path_of(&["library", "event", "project", "sequence", "spine"]);

    // Every id is unique, and every ref names a resource: a dangling ref refuses the import.
    let resources = path_of(&["resources"]);
    let mut ids = std::collections::BTreeMap::new();
    for resource in resources.children().filter(roxmltree::Node::is_element) {
        let id = resource.attribute("id").expect("every resource has an id");
        assert!(ids.insert(id, resource).is_none(), "id {id} is unique");
    }
    for node in document.descendants().filter(|n| n.attribute("ref").is_some()) {
        let reference = node.attribute("ref").unwrap_or_default();
        assert!(ids.contains_key(reference), "<{}> ref {reference} names a resource", node.tag_name().name());
    }
    let format = ids.get(sequence.attribute("format").expect("the sequence names its format")).expect("its format is a resource");
    assert_eq!(format.tag_name().name(), "format");
    let frame = attr(*format, "frameDuration");
    assert!(format.attribute("width").and_then(|w| w.parse::<u32>().ok()).is_some_and(|w| w > 0));
    assert!(format.attribute("height").and_then(|h| h.parse::<u32>().ok()).is_some_and(|h| h > 0));

    // Every time is a rational time in seconds.
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        for name in TIMES {
            if let Some(text) = node.attribute(name) {
                assert!(time(text).is_some(), "<{}> {name}={text:?} is a rational time", node.tag_name().name());
            }
        }
    }

    // Each asset names one file in the archive, relative to the document and inside its folder.
    let mut assets = std::collections::BTreeMap::new();
    for asset in resources.children().filter(|n| n.has_tag_name("asset")) {
        let reps: Vec<_> = asset.children().filter(|n| n.has_tag_name("media-rep")).collect();
        assert_eq!(reps.len(), 1, "one media-rep per asset");
        assert_eq!(reps[0].attribute("kind"), Some("original-media"));
        let src = reps[0].attribute("src").expect("a media-rep has a src");
        assert!(!src.contains("://") && !src.starts_with('/') && !src.split('/').any(|part| part == ".."), "{src} stays inside the folder");
        assert!(names.contains(&decoded(src)), "{src} is in the archive ({names:?})");
        assets.insert(asset.attribute("id").unwrap_or_default(), (attr(asset, "start"), attr(asset, "duration")));
    }

    // The spine plays its items one after another on the frame grid, and the sequence is as long
    // as they are: an editor places spine items by offset, so a gap or overlap moves every cut.
    let mut end = (0, 1);
    let mut items = 0;
    let elements: Vec<_> = spine.children().filter(roxmltree::Node::is_element).collect();
    for (n, item) in elements.iter().copied().enumerate() {
        let name = item.tag_name().name();
        if name == "transition" {
            // A transition sits between two clips, centred on their cut, and moves nothing.
            let (offset, duration) = (attr(item, "offset"), attr(item, "duration"));
            assert!(whole_frames(offset, frame) && whole_frames(duration, frame), "a transition is on the frame grid");
            let clip = |node: Option<&roxmltree::Node<'_, '_>>| node.is_some_and(|node| CLIPS.contains(&node.tag_name().name()));
            assert!(clip(n.checked_sub(1).and_then(|before| elements.get(before))) && clip(elements.get(n + 1)), "a transition is between two clips");
            assert!(same(add(offset, (duration.0, duration.1 * 2)), end), "a transition is centred on the cut before it");
            continue;
        }
        assert!(name == "gap" || CLIPS.contains(&name), "<{name}> belongs on a spine");
        assert!(item.attribute("lane").is_none(), "spine items have no lane");
        let (offset, start, duration) = (attr(item, "offset"), attr(item, "start"), attr(item, "duration"));
        assert!(same(offset, end), "<{name}> starts where the one before it ends");
        // `start` is source time: whole frames is this writer's invariant (one rate everywhere),
        // not an editor's rule.
        for t in [offset, start, duration] {
            assert!(whole_frames(t, frame), "<{name}> times are whole frames");
        }
        assert!(duration.0 > 0, "<{name}> lasts");
        end = add(offset, duration);
        items += 1;
        // Connected clips sit on a lane, inside their spine item, within its own time.
        for child in item.children().filter(|n| CLIPS.contains(&n.tag_name().name())) {
            let lane: i32 = child.attribute("lane").and_then(|l| l.parse().ok()).expect("a connected clip has a lane");
            assert_ne!(lane, 0, "lane 0 is the spine");
            assert!(attr(child, "duration").0 > 0);
            let at = attr(child, "offset");
            assert!(whole_frames(at, frame) && whole_frames(attr(child, "duration"), frame), "connected clips are on the frame grid");
            // A connected clip's offset is in its parent's time: it starts within the parent.
            let (from, to) = (start, add(start, duration));
            assert!(at.0 * from.1 >= from.0 * at.1 && at.0 * to.1 < to.0 * at.1, "<{}> starts within its spine item", child.tag_name().name());
        }
        // Clips play within their media, except stills (no length) and titles (no media).
        for clip in std::iter::once(item).chain(item.children()).filter(|n| matches!(n.tag_name().name(), "asset-clip" | "video" | "audio")) {
            let Some(&(media_start, length)) = clip.attribute("ref").and_then(|r| assets.get(r)) else { continue };
            if length.0 == 0 {
                continue;
            }
            let (from, played) = (attr(clip, "start"), add(attr(clip, "start"), attr(clip, "duration")));
            let media_end = add(media_start, length);
            assert!(from.0 * media_start.1 >= media_start.0 * from.1, "<{}> starts within its media", clip.tag_name().name());
            assert!(played.0 * media_end.1 <= media_end.0 * played.1, "<{}> {:?} plays within its media", clip.tag_name().name(), clip.attribute("name"));
        }
    }
    assert!(items >= 2, "the representative cut has several spine items");
    assert!(same(attr(sequence, "duration"), end), "the sequence is as long as its spine");
    xml
}

#[test]
fn rational_times_parse_and_compare_as_editors_read_them() {
    assert_eq!(time("48/30s"), Some((48, 30)));
    assert_eq!(time("2s"), Some((2, 1)));
    for bad in ["48/30", "1/0s", "-1/30s", "s", "1.5s", ""] {
        assert_eq!(time(bad), None, "{bad}");
    }
    assert!(same((48, 30), (8, 5)));
    assert!(whole_frames((48, 30), (1, 30)) && !whole_frames((1, 60), (1, 30)));
    assert_eq!(decoded("media/01-Caf%C3%A9%20au%20lait.mp4"), "media/01-Café au lait.mp4");
}
