use super::*;
use proptest::prelude::*;

const FRAME: Frame = Frame { width: 1920, height: 1080, background: [255, 255, 255, 255] };

fn text_layer(id: &str) -> Layer {
    Layer {
        id: id.into(),
        name: "Text".into(),
        visible: true,
        locked: false,
        opacity: 1.0,
        x: 100.0,
        y: 100.0,
        width: 200.0,
        height: 50.0,
        rotation: 0.0,
        flip_x: false,
        flip_y: false,
        content: Content::Text {
            text: "Hi".into(),
            font: layers::BUILTIN_FONT.into(),
            size: 20.0,
            line_spacing: 1.0,
            tracking: 0.0,
            align: Align::Left,
            color: "#000000".into(),
        },
    }
}

#[test]
fn a_deck_round_trips_with_the_fields_and_layers_it_does_not_know() {
    let json = r##"{"version":1,"theme":"dark","slides":[{"id":"s1","name":"One","notes":"say hi","layers":[
        {"id":"t","name":"T","x":10,"y":10,"width":20,"height":20,"content":{"kind":"text","text":"x","size":12,"color":"#000000"}},
        {"id":"v","name":"Video","x":0,"y":0,"width":1,"height":1,"content":{"kind":"video","clip":"abc"}},
        {"id":"o","name":"Too faint","opacity":7,"x":0,"y":0,"width":1,"height":1,"content":{"kind":"shape","shape":"ellipse"}}
    ]}]}"##;
    let deck = Deck::parse(json).unwrap();
    let slide = &deck.slides[0];
    assert!(matches!(slide.layers[0], Item::Layer(_)));
    assert!(matches!(slide.layers[1], Item::Unsupported(_)), "an unknown kind is kept, not read");
    assert!(matches!(slide.layers[2], Item::Unsupported(_)), "a layer out of range is kept, not read");
    assert_eq!(slide.extra.get("notes"), Some(&Value::from("say hi")));
    let again: Value = serde_json::from_str(&deck.encode().unwrap()).unwrap();
    let before: Value = serde_json::from_str(json).unwrap();
    // Unknown fields and unreadable layers come back exactly; a read layer comes back as read.
    for path in ["/theme", "/slides/0/notes", "/slides/0/layers/1", "/slides/0/layers/2"] {
        assert_eq!(again.pointer(path), before.pointer(path), "{path} is kept");
    }
    assert_eq!(Deck::parse(&deck.encode().unwrap()).unwrap(), deck);
}

#[test]
fn a_deck_from_a_newer_version_is_read_only() {
    let deck = Deck::parse(r#"{"version":9,"slides":[{"whatever":true}],"pages":3}"#).unwrap();
    assert_eq!(deck.version, 9);
    assert!(!deck.writable());
    assert!(deck.slides.is_empty());
    assert!(deck.encode().is_err());
    assert!(Deck::parse(r#"{"version":"two"}"#).is_err());
}

#[test]
fn an_empty_or_versionless_deck_is_version_one() {
    let deck = Deck::parse("{}").unwrap();
    assert_eq!(deck, Deck::default());
    assert_eq!(deck.encode().unwrap(), r#"{"version":1,"slides":[]}"#);
}

#[test]
fn slides_and_layers_need_unique_valid_ids_and_bounded_counts() {
    let mut deck = Deck::default();
    deck.slides.push(Slide::new("a", "A"));
    deck.slides.push(Slide::new("a", "B"));
    assert!(deck.check().is_err());
    deck.slides[1].id = "bad id".into();
    assert!(deck.check().is_err());
    deck.slides[1].id = "b".into();
    deck.slides[1].layers = vec![Item::Layer(text_layer("t")), Item::Unsupported(serde_json::json!({"id": "t"}))];
    assert!(deck.check().is_err(), "an unsupported layer's id is taken too");
    deck.slides[1].layers.pop();
    deck.check().unwrap();
    deck.slides[1].layers = (0..=MAX_LAYERS).map(|n| Item::Layer(text_layer(&format!("t{n}")))).collect();
    assert!(deck.check().is_err());
    deck.slides = (0..=MAX_SLIDES).map(|n| Slide::new(format!("s{n}"), "")).collect();
    assert!(deck.check().is_err());
    assert_eq!(Deck::parse(&" ".repeat(MAX_ENCODED_BYTES + 1)), Err(ImageError::RequestLimit));
}

#[test]
fn an_image_layer_is_not_a_deck_layer() {
    let mut layer = text_layer("i");
    layer.content = Content::Image { input: 0, fit: true };
    assert!(usable(&layer).is_err());
}

#[test]
fn sources_draw_what_their_node_shows() {
    let mut slide = node_slide("s".into(), "Cat", "n1", FRAME);
    slide.layers.push(Item::Layer(source_layer("n2", "Prompt", (500.0, 500.0), (400.0, 200.0))));
    slide.layers.push(Item::Layer(source_layer("gone", "Gone", (10.0, 10.0), (10.0, 10.0))));
    let mut hidden = text_layer("hidden");
    hidden.visible = false;
    slide.layers.push(Item::Layer(hidden));
    slide.layers.push(Item::Unsupported(serde_json::json!({"id": "x"})));
    let drawn = drawable(&slide, FRAME, |origin, _| match origin {
        "n1" => Shown::Image("cat.png"),
        "n2" => Shown::Text("a cat in a hat, sitting on a mat by the door".into()),
        _ => Shown::Missing,
    });
    assert_eq!(drawn.images, vec!["cat.png"]);
    assert_eq!(drawn.missing, vec!["src-gone".to_owned()]);
    assert_eq!(drawn.layers.len(), 3, "hidden and unsupported layers aren't drawn");
    assert!(matches!(drawn.layers[0].content, Content::Image { input: 0, fit: true }));
    let Content::Text { text, color, .. } = &drawn.layers[1].content else { panic!("text source draws text") };
    assert!(text.contains("cat") && color == "#000000");
    assert!(matches!(drawn.layers[2].content, Content::Source { .. }), "a missing source stays an empty box");
    for layer in &drawn.layers {
        layer.check().unwrap();
    }
}

#[test]
fn a_field_slide_keeps_its_nodes_where_they_sit() {
    let members = vec![("a".to_owned(), "A".to_owned(), (0.0, 0.0)), ("b".to_owned(), "B".to_owned(), (400.0, 0.0))];
    let slide = field_slide("f".into(), "Moodboard", &members, FRAME);
    slide.check().unwrap();
    let layers: Vec<&Layer> = slide.layers.iter().filter_map(Item::layer).collect();
    assert_eq!(layers.len(), 3);
    assert!(matches!(layers[0].content, Content::Text { .. }), "the title goes first");
    assert!(layers[1].x < layers[2].x && (layers[1].y - layers[2].y).abs() < 1e-9);
    for layer in &layers {
        let (l, t, r, b) = layer.bounds();
        assert!(l >= 0.0 && t >= 0.0 && r <= 1920.0 && b <= 1080.0, "{layer:?} stays in the frame");
    }
    assert_eq!(field_slide("e".into(), "", &[], FRAME).layers.len(), 0);
}

proptest! {
    #[test]
    fn parsing_anything_never_panics(json in ".{0,400}") {
        if let Ok(deck) = Deck::parse(&json) {
            let _ = deck.encode();
        }
    }

    #[test]
    fn text_sources_always_make_valid_layers(text in ".{0,3000}", w in 1.0f64..5000.0, h in 1.0f64..5000.0) {
        let mut slide = Slide::new("s", "");
        slide.layers.push(Item::Layer(source_layer("n", "N", (0.0, 0.0), (w, h))));
        let drawn = drawable(&slide, FRAME, |_, _| Shown::<()>::Text(text.clone()));
        for layer in &drawn.layers {
            prop_assert!(layer.check().is_ok());
        }
    }
}
