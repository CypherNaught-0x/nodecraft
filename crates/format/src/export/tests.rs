//! The hostile-input half of export: a template, a title or a format request must never be able to
//! choose a path, invent a conversion, or leave a reader guessing which happened.
use super::*;
use nodecraft_types::{Blake3, NodeId};

fn fields<'a>(title: &'a str, output: &'a str) -> NameFields<'a> {
    NameFields {
        index: 1,
        node: NodeId::from_bytes([0xab; 16]),
        generation: Some(GenerationId::from_bytes([0xcd; 16])),
        kind: "generation.image",
        title,
        output,
        item: None,
        hash: None,
    }
}

fn stem(title: &str, template: &str) -> String {
    Template::parse(template).expect("template").render(&fields(title, "out"), "image/png").expect("render").stem
}

/// The property every case below defends: whatever a caller types, the name stays one component of
/// the folder it was given.
#[test]
fn a_hostile_template_can_never_name_a_path_outside_the_folder() {
    for template in [
        "../../../../etc/passwd",
        "..\\..\\windows\\system32\\config",
        "{title}/../../escape",
        "{title}\\{output}",
        "/absolute/from/template",
        "a/../../b",
        "..",
        ".",
        "...",
    ] {
        if template.contains(['/', '\\']) || template.contains("..") {
            let error = Template::parse(template).expect_err("path syntax is never a template");
            assert_eq!(error.kind, ErrorKind::InvalidInput);
            assert!(error.message.contains("path separators"), "{error:?}");
        } else {
            let rendered = stem("safe", template);
            assert!(!rendered.contains('/') && !rendered.contains('\\'), "{template:?} produced {rendered:?}");
            assert!(!rendered.is_empty(), "{template:?} produced an empty name");
        }
    }
}

/// Control characters are refused outright rather than quietly becoming `_`: a name that absorbs one
/// is harder to notice in a folder than a parameter error that says what to change.
#[test]
fn a_template_with_a_control_character_is_refused_not_silently_repaired() {
    for template in ["\0hidden", "line\nbreak", "tab\tname", "\u{7}"] {
        assert_eq!(Template::parse(template).unwrap_err().kind, ErrorKind::InvalidInput, "{template:?}");
    }
}

/// The same property for the *values* a template interpolates, which come from a project file a
/// person did not necessarily write.
#[test]
fn a_hostile_title_cannot_escape_either() {
    for title in ["../../../../etc/passwd", "..\\..\\escape.png", "a/b/c", "\u{0}nul", "trailing.", "trailing space ", "  ", "", ".."] {
        if title.contains(['/', '\\']) || title.contains("..") {
            let error = Template::parse("{title}").expect("template").render(&fields(title, "out"), "image/png").expect_err("unsafe document name");
            assert_eq!(error.kind, ErrorKind::InvalidInput);
            assert!(error.message.contains("rename"));
        } else {
            let rendered = stem(title, "{title}");
            assert!(!rendered.ends_with('.') && !rendered.ends_with(' '), "{title:?} produced {rendered:?}");
            assert!(!rendered.is_empty(), "{title:?} produced an empty name");
        }
    }
}

#[test]
fn an_empty_or_unusable_template_is_refused_rather_than_guessed() {
    assert_eq!(Template::parse("").unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(Template::parse("\u{0}").unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(Template::parse("{node").unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(Template::parse("node}").unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(Template::parse("{node{index}}").unwrap_err().kind, ErrorKind::InvalidInput);
    // An unknown token is an error: silently dropping it would quietly collide every file.
    assert_eq!(Template::parse("{prompt}").unwrap_err().kind, ErrorKind::InvalidInput);
    // A token the person did not ask for is never invented: a missing `{item}` contributes nothing,
    // and the literal text the person typed stays exactly as typed.
    assert_eq!(Template::parse("{title}-{item}").unwrap().render(&fields("hero", "out"), "image/png").unwrap().stem, "hero-");
    assert_eq!(Template::parse("{title}{item}").unwrap().render(&fields("hero", "out"), "image/png").unwrap().stem, "hero");
}

#[test]
fn an_over_long_template_or_title_stays_bounded_and_whole() {
    let long = "x".repeat(100_000);
    assert_eq!(Template::parse(&long).unwrap_err().kind, ErrorKind::InvalidInput);
    // A huge title is truncated, not rejected, and never longer than the documented bound.
    let rendered = stem(&long, "{title}");
    assert!(rendered.len() <= MAX_NAME_BYTES, "{} bytes", rendered.len());

    // Combining marks are not kept as themselves, so a name cannot carry a half-assembled
    // character or a name that differs only by its normalisation form.
    let combining = "e\u{0301}".repeat(400);
    let rendered = stem(&combining, "{title}");
    assert!(rendered.is_ascii(), "combining marks become `_`: {rendered:?}");
    assert!(rendered.len() <= MAX_NAME_BYTES);

    // An accented title is truncated on a character boundary, never through a multi-byte scalar.
    let accented = "é".repeat(500);
    let rendered = stem(&accented, "{title}");
    assert!(rendered.len() <= MAX_NAME_BYTES);
    assert!(rendered.is_char_boundary(rendered.len()));
    assert!(rendered.chars().all(|c| c == 'é'));
}

#[test]
fn a_windows_reserved_name_is_not_written_verbatim() {
    for reserved in ["con", "NUL", "aux.txt", "com1", "com10", "LPT9"] {
        assert!(stem(reserved, "{title}").starts_with('_'), "{reserved} would be written as-is");
    }
    // An ordinary name that merely starts with the same letters is left alone.
    assert_eq!(stem("contacts", "{title}"), "contacts");
    assert_eq!(stem("concert", "{title}"), "concert");
    assert_eq!(stem("control.png", "{title}"), "control.png");
}

#[test]
fn a_numbered_collision_name_is_bounded_and_stays_one_component() {
    let long = ExportName { stem: stem(&"y".repeat(500), "{title}"), extension: Some("png".to_owned()) };
    for suffix in [2_u32, 999] {
        let named = long.with_suffix(suffix);
        assert!(named.len() <= MAX_NAME_BYTES + MAX_EXTENSION_BYTES + 8, "{named}");
        assert!(!named.contains('/'), "{named}");
        assert!(named.ends_with(".png"), "{named} keeps its extension");
    }
}

#[test]
fn the_extension_follows_the_stored_type_and_invents_nothing() {
    assert_eq!(extension_for("image/png").as_deref(), Some("png"));
    assert_eq!(extension_for("image/jpeg").as_deref(), Some("jpg"));
    assert_eq!(extension_for("image/svg+xml").as_deref(), Some("svg"));
    assert_eq!(extension_for("video/mp4").as_deref(), Some("mp4"));
    assert_eq!(extension_for("model/stl").as_deref(), Some("stl"));
    assert_eq!(extension_for("model/ply").as_deref(), Some("ply"));
    assert_eq!(extension_for("model/stl; profile=binary").as_deref(), Some("stl"));
    assert_eq!(extension_for("text/plain").as_deref(), Some("txt"));
    assert_eq!(extension_for("text/plain; charset=utf-8").as_deref(), Some("txt"));
    // An unknown but a single lowercase subtype is written as itself…
    assert_eq!(extension_for("image/avif").as_deref(), Some("avif"));
    assert_eq!(extension_for("image/heic").as_deref(), Some("heic"));
    // …and anything that is not one usable token gets no extension rather than a mangling of it:
    // stripping a tree prefix, or cutting a long subtype, would invent a type the result does not
    // have. A name without an extension is the honest answer.
    assert_eq!(extension_for("application/x-some-very-long-thing"), None);
    assert_eq!(extension_for("application/octet-stream"), None);
    assert_eq!(extension_for("text/vnd.a"), None);
    assert_eq!(extension_for("image/svg+xml"), Some("svg".to_owned()), "the +suffix is not the extension");
    assert_eq!(extension_for(""), None);
}

#[test]
fn pass_through_is_the_only_operation_and_it_is_named_as_such() {
    assert_eq!(FormatRequest::default().action().unwrap(), FormatAction::PassThrough);
    assert_eq!(FormatAction::PassThrough.name(), "passthrough");
    // A conversion is refused, in terms a person can act on, and never writes the wrong bytes.
    let convert = FormatRequest { mode: FormatMode::Convert, mime: Some("image/jpeg".to_owned()) };
    assert_eq!(convert.validate(), Ok(()));
    let error = convert.action().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("image/jpeg"), "{error:?}");
    assert!(error.message.contains("not implemented"), "{error:?}");
    assert!(error.message.contains("original"), "{error:?}");
    // Choosing SVG is not a free conversion.
    let vectorise = FormatRequest { mode: FormatMode::Vectorise, mime: Some("image/svg+xml".to_owned()) };
    let error = vectorise.action().unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("separate quoted operation"), "{error:?}");
    // An SVG *source* is a different operation: it passes through, unchanged.
    assert_eq!(extension_for("image/svg+xml").as_deref(), Some("svg"));
}

#[test]
fn a_hostile_mime_never_reaches_a_message() {
    for mime in ["IMAGE/JPEG", "image/jpeg; charset=/etc/passwd", "a/", "/b", "image/jpeg\u{0}"] {
        let request = FormatRequest { mode: FormatMode::Convert, mime: Some(mime.to_owned()) };
        assert_eq!(request.validate().unwrap_err().kind, ErrorKind::InvalidInput, "{mime:?}");
    }
    // A type longer than the documented bound is refused on length, before anything can echo it.
    let huge = FormatRequest { mode: FormatMode::Convert, mime: Some("image/".to_owned() + &"x".repeat(200)) };
    assert_eq!(huge.validate().unwrap_err().kind, ErrorKind::InvalidInput);
    // A mode that needs a type and is not given one is a parameter error, not a guess.
    assert_eq!(FormatRequest { mode: FormatMode::Convert, mime: None }.validate().unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(FormatRequest { mode: FormatMode::Original, mime: Some("image/png".to_owned()) }.validate().unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn collision_policy_names_are_the_documented_three() {
    assert_eq!(Collision::default(), Collision::Skip);
    assert_eq!(Collision::Skip.name(), "skip");
    assert_eq!(Collision::Overwrite.name(), "overwrite");
    assert_eq!(Collision::Numbered.name(), "numbered");
    assert_eq!(serde_json::from_str::<Collision>("\"numbered\"").unwrap(), Collision::Numbered);
    assert!(serde_json::from_str::<Collision>("\"silent\"").is_err());
    assert!(serde_json::from_str::<FormatRequest>("{\"mode\":\"original\",\"extra\":1}").is_err());
    assert_eq!(serde_json::from_str::<FormatRequest>("{\"mode\":\"original\"}").unwrap(), FormatRequest::default());
}

#[test]
fn a_source_id_names_the_node_generation_port_and_item() {
    let source = ExportSource {
        canvas: CanvasId::from_bytes([1; 16]),
        node: NodeId::from_bytes([2; 16]),
        port: "out".to_owned(),
        item: Some(3),
        generation: Some(GenerationId::from_bytes([4; 16])),
    };
    let named = source.stable_id();
    assert!(named.contains(&source.node.to_string()), "{named}");
    assert!(named.contains(&source.generation.unwrap().to_string()), "{named}");
    assert!(named.ends_with("#3"), "{named}");
    // A different generation is a different mapping: which generation a file came from is visible.
    let older = ExportSource { generation: Some(GenerationId::from_bytes([5; 16])), ..source.clone() };
    assert_ne!(named, older.stable_id());
    assert!(!ExportSource { item: None, ..source }.stable_id().ends_with('#'));
}

#[test]
fn a_hash_token_names_the_content_in_eight_characters() {
    let hash = Blake3::from_bytes([0x1b; 32]);
    let rendered = Template::parse("{hash}").unwrap().render(&NameFields { hash: Some(hash), ..fields("t", "out") }, "image/png").unwrap();
    assert_eq!(rendered.stem, "1b1b1b1b");
    assert_eq!(rendered.extension.as_deref(), Some("png"));
    // No content, no invented hash.
    assert_eq!(Template::parse("{hash}").unwrap().render(&NameFields { hash: None, ..fields("t", "out") }, "image/png").unwrap().stem, "export");
}

#[test]
fn a_media_value_becomes_a_reference_or_a_reason_never_a_guess() {
    use nodecraft_types::{AssetRef, MediaMeta, Value};
    let bytes: &[u8] = b"\x89PNG\r\n\x1a\nthe stored image";
    let mut store = crate::AssetStore::default();
    let hash = store.insert(bytes).expect("insert");
    let asset = AssetRef { hash, mime: "image/png".parse().expect("mime"), meta: MediaMeta::default() };

    let (held, mime, size, named) = media_for(&store, &Value::Media(asset)).expect("a media result is its stored bytes");
    assert_eq!(held.hash, hash, "the reference is kept, not re-encoded — the write is the store's job");
    assert_eq!(mime, "image/png");
    assert_eq!(size, bytes.len() as u64, "the length the plan reports is the stored length");
    assert_eq!(named, Some(hash));

    // A text result is refused, as `history.export` refuses it (#557): copying is the way out, and
    // a batch that wrote `.txt` would contradict the shipped decision rather than extend it.
    assert_eq!(media_for(&store, &Value::Text("a line".into())).unwrap_err(), reasons::UNSUPPORTED_VALUE);

    // A reference to bytes the project no longer holds is a reason, not an empty file.
    let gone = AssetRef { hash: Blake3::from_bytes([9; 32]), mime: "image/png".parse().expect("mime"), meta: MediaMeta::default() };
    assert_eq!(media_for(&store, &Value::Media(gone)).unwrap_err(), reasons::READING_ASSET);
    assert_eq!(media_for(&store, &Value::Bool(true)).unwrap_err(), reasons::UNSUPPORTED_VALUE);
    assert_eq!(media_for(&store, &Value::List(vec![Value::Text("a".into())])).unwrap_err(), reasons::UNSUPPORTED_VALUE);
}
