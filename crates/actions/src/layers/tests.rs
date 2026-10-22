use super::*;

const WHITE: [u8; 4] = [255, 255, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn frame(width: u32, height: u32, background: [u8; 4]) -> Frame {
    Frame { width, height, background }
}

fn layer(id: &str, x: f64, y: f64, width: f64, height: f64, content: Content) -> Layer {
    Layer {
        id: id.into(),
        name: id.into(),
        visible: true,
        locked: false,
        opacity: 1.0,
        x,
        y,
        width,
        height,
        rotation: 0.0,
        flip_x: false,
        flip_y: false,
        content,
    }
}

fn rect(fill: &str) -> Content {
    Content::Shape { shape: ShapeKind::Rectangle, fill: Some(fill.into()), stroke: None, stroke_width: 0.0 }
}

fn text(text: &str, size: f64, align: Align) -> Content {
    styled(text, BUILTIN_FONT, size, 0.0, align)
}

fn styled(text: &str, font: &str, size: f64, tracking: f64, align: Align) -> Content {
    Content::Text { text: text.into(), font: font.into(), size, line_spacing: 1.0, tracking, align, color: "#000000".into() }
}

fn at(image: &Raster, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width() + x) * 4) as usize;
    image.pixels()[i..i + 4].try_into().unwrap()
}

/// A `w`×`h` image whose left half is red and right half blue.
fn halves(w: u32, h: u32) -> Raster {
    let mut pixels = Vec::new();
    for _ in 0..h {
        for x in 0..w {
            pixels.extend(if x < w / 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        }
    }
    Raster::new(w, h, pixels, ImageLimits::default()).unwrap()
}

fn draw(frame: Frame, layers: Vec<Layer>, images: &[Raster]) -> Raster {
    let images: Vec<Option<Raster>> = images.iter().cloned().map(Some).collect();
    composite(frame, &Layers { layers }, &images, &Fonts::default(), 1.0, ImageLimits::default()).unwrap()
}

#[test]
fn layers_stack_bottom_first_and_hidden_ones_are_skipped() {
    let out =
        draw(frame(200, 100, WHITE), vec![layer("a", 50.0, 50.0, 100.0, 100.0, rect("#ff0000")), layer("b", 100.0, 50.0, 100.0, 100.0, rect("#0000ff"))], &[]);
    assert_eq!((out.width(), out.height()), (200, 100));
    assert_eq!(at(&out, 20, 50), [255, 0, 0, 255]);
    assert_eq!(at(&out, 75, 50), [0, 0, 255, 255]);
    assert_eq!(at(&out, 190, 50), WHITE);

    let mut hidden = layer("b", 100.0, 50.0, 100.0, 100.0, rect("#0000ff"));
    hidden.visible = false;
    let out = draw(frame(200, 100, CLEAR), vec![layer("a", 50.0, 50.0, 100.0, 100.0, rect("#ff0000")), hidden], &[]);
    assert_eq!(at(&out, 75, 50), [255, 0, 0, 255]);
    assert_eq!(at(&out, 190, 50), CLEAR);
}

#[test]
fn opacity_blends_and_outlines_over_fills_do_not_double_up() {
    let mut half = layer(
        "a",
        50.0,
        50.0,
        100.0,
        100.0,
        Content::Shape { shape: ShapeKind::Rectangle, fill: Some("#000000".into()), stroke: Some("#000000".into()), stroke_width: 10.0 },
    );
    half.opacity = 0.5;
    let out = draw(frame(100, 100, WHITE), vec![half], &[]);
    let middle = at(&out, 50, 50)[0];
    let edge = at(&out, 1, 50)[0];
    assert!((120..=135).contains(&middle), "{middle}");
    assert_eq!(middle, edge);
}

#[test]
fn images_fill_their_box_and_turn_and_mirror_with_it() {
    let image = halves(10, 10);
    let place = |rotation: f64, flip_x: bool| {
        let mut l = layer("img", 100.0, 100.0, 100.0, 100.0, Content::Image { input: 0, fit: false });
        l.rotation = rotation;
        l.flip_x = flip_x;
        draw(frame(200, 200, CLEAR), vec![l], std::slice::from_ref(&image))
    };
    let plain = place(0.0, false);
    assert_eq!(at(&plain, 70, 100), [255, 0, 0, 255]);
    assert_eq!(at(&plain, 130, 100), [0, 0, 255, 255]);
    assert_eq!(at(&plain, 30, 100), CLEAR);
    // Mirrored, the halves swap.
    let flipped = place(0.0, true);
    assert_eq!(at(&flipped, 70, 100), [0, 0, 255, 255]);
    // A quarter turn clockwise puts the left half on top.
    let turned = place(90.0, false);
    assert_eq!(at(&turned, 100, 70), [255, 0, 0, 255]);
    assert_eq!(at(&turned, 100, 130), [0, 0, 255, 255]);
}

#[test]
fn shapes_are_rectangles_ellipses_and_triangles() {
    let shape = |shape| Content::Shape { shape, fill: Some("#000000".into()), stroke: None, stroke_width: 0.0 };
    let out = draw(
        frame(300, 100, WHITE),
        vec![
            layer("r", 50.0, 50.0, 100.0, 100.0, shape(ShapeKind::Rectangle)),
            layer("e", 150.0, 50.0, 100.0, 100.0, shape(ShapeKind::Ellipse)),
            layer("t", 250.0, 50.0, 100.0, 100.0, shape(ShapeKind::Triangle)),
        ],
        &[],
    );
    // Corners: the rectangle covers them, the ellipse and triangle don't.
    assert_eq!(at(&out, 3, 3), [0, 0, 0, 255]);
    assert_eq!(at(&out, 103, 3), WHITE);
    assert_eq!(at(&out, 203, 3), WHITE);
    assert_eq!(at(&out, 150, 50), [0, 0, 0, 255]);
    // The triangle's apex is at the top middle, its base along the bottom.
    assert!(at(&out, 250, 5)[0] < 255);
    assert_eq!(at(&out, 203, 97), [0, 0, 0, 255]);
}

#[test]
fn text_draws_inside_its_box_and_aligns() {
    let content = text("Hi", 40.0, Align::Left);
    let (w, h) = text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, content.clone()), &Fonts::default()).unwrap();
    assert!(w > 20.0 && w < 80.0 && h > 40.0 && h < 60.0, "{w} {h}");
    let ink = |out: &Raster, x0: u32, x1: u32| (x0..x1).any(|x| (0..out.height()).any(|y| at(out, x, y)[0] < 128));
    for (align, inked) in [(Align::Left, (true, false)), (Align::Right, (false, true))] {
        let out = draw(frame(400, 100, WHITE), vec![layer("t", 200.0, 50.0, 400.0, 100.0, text("Hi", 40.0, align))], &[]);
        assert_eq!((ink(&out, 0, 100), ink(&out, 300, 400)), inked, "{align:?}");
    }
    let two = text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, text("Hi\nHi", 40.0, Align::Left)), &Fonts::default()).unwrap();
    assert!((two.1 - 2.0 * h).abs() <= 2.0, "{two:?}");
    // Tracking spreads the letters.
    let wide = styled("Hi", BUILTIN_FONT, 40.0, 1.0, Align::Left);
    assert!(text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, wide), &Fonts::default()).unwrap().0 > w + 30.0);
}

#[test]
fn an_unknown_font_falls_back_to_the_bundled_one() {
    let other = styled("Hi", "no such font", 40.0, 0.0, Align::Left);
    let builtin = text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, text("Hi", 40.0, Align::Left)), &Fonts::default()).unwrap();
    assert_eq!(text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, other), &Fonts::default()).unwrap(), builtin);
}

#[test]
fn a_preview_scales_the_whole_frame() {
    let layers = Layers { layers: vec![layer("a", 50.0, 50.0, 100.0, 100.0, rect("#ff0000"))] };
    let out = composite(frame(400, 200, WHITE), &layers, &[], &Fonts::default(), 0.25, ImageLimits::default()).unwrap();
    assert_eq!((out.width(), out.height()), (100, 50));
    assert_eq!(at(&out, 10, 10), [255, 0, 0, 255]);
    assert_eq!(at(&out, 40, 10), WHITE);
}

#[test]
fn stored_layers_round_trip_and_hostile_ones_are_refused() {
    let mut l = layer("src-1", 10.0, 20.0, 30.0, 40.0, Content::Source { origin: "n1".into(), nth: 0, fit: false });
    l.rotation = 15.0;
    let layers = Layers { layers: vec![l, layer("txt", 1.0, 1.0, 1.0, 1.0, text("a", 12.0, Align::Center))] };
    let json = layers.encode().unwrap();
    assert!(!json.contains("visible") && !json.contains("opacity"), "{json}");
    assert_eq!(Layers::parse(&json).unwrap(), layers);
    let base = r#""id":"a","name":"a","x":0,"y":0,"width":10,"height":10"#;
    for bad in [
        format!(r#"{{"layers":[{{{base},"content":{{"kind":"shape","shape":"rectangle"}},"extra":1}}]}}"#),
        format!(r#"{{"layers":[{{{base},"content":{{"kind":"shape","shape":"star"}}}}]}}"#),
        format!(r#"{{"layers":[{{{base},"content":{{"kind":"shape","shape":"rectangle","fill":"red"}}}}]}}"#),
        format!(r#"{{"layers":[{{{base},"opacity":2,"content":{{"kind":"shape","shape":"rectangle"}}}}]}}"#),
        format!(r##"{{"layers":[{{{base},"content":{{"kind":"text","text":"a","size":0,"color":"#000000"}}}}]}}"##),
        format!(r##"{{"layers":[{{{base},"content":{{"kind":"text","text":"a","size":10,"color":"#000000","bold":true}}}}]}}"##),
        r#"{"layers":[{"id":"a b","name":"a","x":0,"y":0,"width":10,"height":10,"content":{"kind":"image","input":0}}]}"#.into(),
        r#"{"layers":[{"id":"a","name":"a","x":0,"y":0,"width":0,"height":10,"content":{"kind":"image","input":0}}]}"#.into(),
        r#"{"layers":"#.into(),
    ] {
        assert!(Layers::parse(&bad).is_err(), "{bad}");
    }
    let twice = Layers { layers: vec![layer("a", 0.0, 0.0, 1.0, 1.0, rect("#000000")), layer("a", 0.0, 0.0, 1.0, 1.0, rect("#000000"))] };
    assert!(twice.check().is_err());
    let many = Layers { layers: (0..=MAX_LAYERS).map(|i| layer(&format!("l{i}"), 0.0, 0.0, 1.0, 1.0, rect("#000000"))).collect() };
    assert!(many.check().is_err());
    assert_eq!(Layers::parse(&" ".repeat(MAX_ENCODED_BYTES + 1)), Err(ImageError::RequestLimit));
    let mut nan = layer("a", 0.0, 0.0, 1.0, 1.0, rect("#000000"));
    nan.x = f64::NAN;
    assert!(nan.check().is_err());
    assert_eq!(parse_color("#11223344").unwrap(), [0x11, 0x22, 0x33, 0x44]);
    for bad in ["#123", "112233", "#11223g", "#ééé"] {
        assert!(parse_color(bad).is_err(), "{bad}");
    }
}

#[test]
fn frames_images_and_work_are_bounded() {
    let layers = Layers::default();
    assert_eq!(composite(frame(99, 100, WHITE), &layers, &[], &Fonts::default(), 1.0, ImageLimits::default()).unwrap_err(), ImageError::Dimensions);
    assert_eq!(composite(frame(4097, 100, WHITE), &layers, &[], &Fonts::default(), 1.0, ImageLimits::default()).unwrap_err(), ImageError::Dimensions);
    assert!(composite(frame(100, 100, WHITE), &layers, &[], &Fonts::default(), 2.0, ImageLimits::default()).is_err());
    assert!(composite(frame(100, 100, WHITE), &layers, &[], &Fonts::default(), f64::NAN, ImageLimits::default()).is_err());
    let missing = Layers { layers: vec![layer("a", 0.0, 0.0, 10.0, 10.0, Content::Image { input: 3, fit: false })] };
    assert_eq!(
        composite(frame(100, 100, WHITE), &missing, &[], &Fonts::default(), 1.0, ImageLimits::default()).unwrap_err(),
        ImageError::Parameter("image layer")
    );
    let unresolved = Layers { layers: vec![layer("a", 0.0, 0.0, 10.0, 10.0, Content::Source { origin: "n".into(), nth: 0, fit: false })] };
    assert!(composite(frame(100, 100, WHITE), &unresolved, &[], &Fonts::default(), 1.0, ImageLimits::default()).is_err());
    let busy = Layers { layers: (0..MAX_LAYERS).map(|i| layer(&format!("l{i}"), 0.0, 0.0, 1.0, 1.0, rect("#000000"))).collect() };
    let tight = ImageLimits { work: 100_000, ..ImageLimits::default() };
    assert_eq!(composite(frame(1000, 1000, WHITE), &busy, &[], &Fonts::default(), 1.0, tight).unwrap_err(), ImageError::WorkLimit);
    let small = ImageLimits { working_bytes: 1_000_000, ..ImageLimits::default() };
    assert_eq!(composite(frame(1000, 1000, WHITE), &layers, &[], &Fonts::default(), 1.0, small).unwrap_err(), ImageError::ScratchLimit);
}

#[test]
fn layers_far_off_the_frame_and_extreme_values_draw_nothing_and_do_not_panic() {
    let mut far = layer("a", MAX_COORD, -MAX_COORD, MAX_COORD, MAX_COORD, rect("#000000"));
    far.rotation = 3600.0;
    let mut thin = layer(
        "b",
        50.0,
        50.0,
        f64::MIN_POSITIVE,
        MAX_COORD,
        Content::Shape { shape: ShapeKind::Triangle, fill: Some("#000000".into()), stroke: Some("#ff0000".into()), stroke_width: MAX_STROKE },
    );
    thin.flip_y = true;
    let empty_text = layer("c", 50.0, 50.0, 10.0, 10.0, text("", 12.0, Align::Center));
    let out = draw(
        frame(100, 100, WHITE),
        vec![far, thin, empty_text, layer("d", 50.0, 50.0, 10.0, 10.0, text("\u{200d}\u{0301}🙂\r\n\t", MAX_TEXT_SIZE, Align::Right))],
        &[],
    );
    assert_eq!((out.width(), out.height()), (100, 100));
}

#[test]
fn stored_sources_follow_their_image_and_new_images_get_fitted_layers_on_top() {
    let f = frame(200, 100, CLEAR);
    let source = |id: &str, origin: &str, nth: u32| layer(id, 50.0, 50.0, 20.0, 20.0, Content::Source { origin: origin.into(), nth, fit: false });
    let stored = Layers { layers: vec![source("b-layer", "b", 0), rect_layer("shape"), source("gone", "z", 0), source("a2", "a", 1)] };
    // Wires reordered: b's image is now input 0, a's second image input 2.
    let sources = ["b".to_owned(), "a".to_owned(), "a".to_owned(), String::new()];
    let resolved = stored.resolve(&sources, f).unwrap();
    let ids: Vec<&str> = resolved.layers.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["b-layer", "shape", "a2", "a-0", "input3-0"]);
    let content: Vec<&Content> = resolved.layers.iter().map(|l| &l.content).collect();
    assert_eq!(content[0], &Content::Image { input: 0, fit: false });
    assert_eq!(content[2], &Content::Image { input: 2, fit: false });
    assert_eq!(content[3], &Content::Image { input: 1, fit: true });
    assert_eq!(content[4], &Content::Image { input: 3, fit: true });
    let auto = &resolved.layers[3];
    assert_eq!((auto.x, auto.y, auto.width, auto.height), (100.0, 50.0, 200.0, 100.0));
    // What the editor lists: the stored layers as they are, plus the automatic ones.
    let listed = stored.with_sources(&sources, f);
    assert_eq!(listed.layers.len(), 6);
    assert_eq!(listed.layers[4].content, Content::Source { origin: "a".into(), nth: 0, fit: true });
    assert_eq!(listed.layers[4].id, auto_id("a", 0));
    // Storing the listed layers keeps an automatic one fitted, and where it was.
    let again = Layers { layers: listed.layers.into_iter().filter(|l| l.id != "gone").collect() }.resolve(&sources, f).unwrap();
    let ids: Vec<&str> = again.layers.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, ["b-layer", "shape", "a2", "a-0", "input3-0"]);
    assert_eq!(again.get("a-0").map(|l| &l.content), Some(&Content::Image { input: 1, fit: true }));
}

fn rect_layer(id: &str) -> Layer {
    layer(id, 10.0, 10.0, 5.0, 5.0, rect("#000000"))
}

#[test]
fn a_fitted_image_keeps_its_proportions() {
    let mut l = layer("img", 100.0, 50.0, 200.0, 100.0, Content::Image { input: 0, fit: true });
    l.name = "fit".into();
    let out = draw(frame(200, 100, CLEAR), vec![l], &[halves(10, 10)]);
    // A square image in a 2:1 box: centred, 100 px wide, clear either side.
    assert_eq!(at(&out, 30, 50), CLEAR);
    assert_eq!(at(&out, 70, 50), [255, 0, 0, 255]);
    assert_eq!(at(&out, 130, 50), [0, 0, 255, 255]);
    assert_eq!(at(&out, 170, 50), CLEAR);
}

#[test]
fn a_layer_renders_alone_upright_with_room_around_its_box() {
    let limits = ImageLimits::default();
    let mut box_layer = layer("r", 500.0, 500.0, 20.0, 10.0, rect("#ff0000"));
    box_layer.rotation = 30.0;
    box_layer.opacity = 0.5;
    let (out, pad) = render_layer(&box_layer, &Fonts::default(), 2.0, limits).unwrap();
    // 1 px of room either side, doubled; rotation, position and opacity are the caller's.
    assert_eq!((pad, out.width(), out.height()), (1.0, 44, 24));
    assert_eq!(at(&out, 22, 12), [255, 0, 0, 255]);
    assert_eq!(at(&out, 0, 0)[3], 0);
    // Text reaches past its box by up to its size, so the room is that.
    let words = layer("t", 0.0, 0.0, 60.0, 20.0, text("Hg", 16.0, Align::Left));
    let (out, pad) = render_layer(&words, &Fonts::default(), 1.0, limits).unwrap();
    assert_eq!(pad, 16.0);
    assert!(out.pixels().as_chunks::<4>().0.iter().any(|p| p[3] > 0), "some ink");
    // Text wider than its box draws past it in the output, so the preview holds all of it.
    let long = layer("t", 0.0, 0.0, 10.0, 20.0, text("a long line of words", 16.0, Align::Left));
    let (tw, _) = text_size(&long, &Fonts::default()).unwrap();
    let (_, pad) = render_layer(&long, &Fonts::default(), 1.0, limits).unwrap();
    assert!(10.0 + pad >= tw, "pad {pad} holds text {tw} wide");
    // Images aren't drawn here, and nothing huge is allocated.
    assert!(render_layer(&layer("i", 0.0, 0.0, 10.0, 10.0, Content::Image { input: 0, fit: false }), &Fonts::default(), 1.0, limits).is_err());
    assert_eq!(render_layer(&layer("big", 0.0, 0.0, 90_000.0, 90_000.0, rect("#000000")), &Fonts::default(), 1.0, limits).unwrap_err(), ImageError::Dimensions);
    assert!(render_layer(&box_layer, &Fonts::default(), f64::NAN, limits).is_err());
}

static LATO_BYTES: &[u8] = include_bytes!("../../../../assets/fonts/Lato-Regular.ttf");

/// Lato with its em halved, so the same text draws twice as large: a font that is told apart from
/// the bundled one by its metrics alone.
fn big_lato() -> Vec<u8> {
    let mut bytes = LATO_BYTES.to_vec();
    let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let head = (0..tables).map(|i| 12 + 16 * i).find(|at| &bytes[*at..*at + 4] == b"head").unwrap();
    let offset = u32::from_be_bytes(bytes[head + 8..head + 12].try_into().unwrap()) as usize;
    let em = u16::from_be_bytes([bytes[offset + 18], bytes[offset + 19]]);
    bytes[offset + 18..offset + 20].copy_from_slice(&(em / 2).to_be_bytes());
    bytes
}

#[test]
fn a_font_file_is_named_by_the_family_it_declares() {
    assert_eq!(font_info(LATO_BYTES).unwrap(), FontInfo { name: "Lato".into(), mime: "font/ttf" });
    for (bytes, why) in
        [(&b"ttcf\0\x01\0\0"[..], "font: a collection (.ttc); use one .ttf or .otf"), (b"\x89PNG\r\n\x1a\n", "font: not a TrueType or OpenType file")]
    {
        assert_eq!(font_info(bytes).unwrap_err(), ImageError::Parameter(why));
    }
    // Cut short: the tables it lists aren't there.
    assert!(font_info(b"OTTO").is_err() && font_info(&LATO_BYTES[..64]).is_err());
    assert!(same_name(" lato ", "Lato") && !same_name("Lato", "Lato Bold"));
}

#[test]
fn a_text_layer_draws_with_the_project_font_it_names_and_falls_back_to_lato() {
    let big = big_lato();
    let mut fonts = Fonts::default();
    fonts.add("Big Lato", &big).unwrap();
    let named = layer("t", 0.0, 0.0, 1.0, 1.0, styled("Hi", "big lato", 40.0, 0.0, Align::Left));
    let plain = layer("t", 0.0, 0.0, 1.0, 1.0, text("Hi", 40.0, Align::Left));
    let (bw, bh) = text_size(&named, &fonts).unwrap();
    let (lw, lh) = text_size(&plain, &fonts).unwrap();
    assert!(bw > lw * 1.8 && bh > lh * 1.8, "{bw}×{bh} vs {lw}×{lh}");
    assert!(fonts.has("Big Lato") && fonts.has("Lato") && !fonts.has("Other"));
    // Without the project's font the layer draws with Lato.
    assert_eq!(text_size(&named, &Fonts::default()).unwrap(), (lw, lh));
    // A later font of the same name replaces the earlier; the count is bounded.
    fonts.add("BIG LATO", LATO_BYTES).unwrap();
    assert_eq!(text_size(&named, &fonts).unwrap(), (lw, lh));
    for n in 1..MAX_FONTS {
        fonts.add(&format!("f{n}"), LATO_BYTES).unwrap();
    }
    assert!(fonts.add("one more", LATO_BYTES).is_err());
    assert!(Fonts::default().add("junk", b"not a font").is_err());
    // A run and a preview draw with it alike.
    let frame = frame(200, 200, WHITE);
    let shown = layer("t", 100.0, 100.0, 180.0, 180.0, styled("Hi", "Big Lato", 40.0, 0.0, Align::Left));
    let mut fonts = Fonts::default();
    fonts.add("Big Lato", &big).unwrap();
    let run = composite(frame, &Layers { layers: vec![shown.clone()] }, &[], &fonts, 1.0, ImageLimits::default()).unwrap();
    let fallback = composite(frame, &Layers { layers: vec![shown.clone()] }, &[], &Fonts::default(), 1.0, ImageLimits::default()).unwrap();
    assert_ne!(run.pixels(), fallback.pixels());
    assert!(render_layer(&shown, &fonts, 1.0, ImageLimits::default()).is_ok());
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig { cases: 96, ..Default::default() })]
    /// A damaged font is refused or drawn, never a panic.
    #[test]
    fn a_damaged_font_never_panics(edits in proptest::collection::vec((0usize..LATO_BYTES.len(), proptest::prelude::any::<u8>()), 1..64), cut in 12usize..LATO_BYTES.len()) {
        let mut bytes = LATO_BYTES.to_vec();
        // Most damage lands in the table directory and the small tables, where parsing starts.
        for (at, value) in edits {
            let at = if at % 2 == 0 { at % 4096 } else { at };
            bytes[at] = value;
        }
        for bytes in [&bytes[..], &bytes[..cut]] {
            let _ = font_info(bytes);
            let mut fonts = Fonts::default();
            if fonts.add("Damaged", bytes).is_ok() {
                let words = layer("t", 50.0, 50.0, 90.0, 90.0, styled("Hi, fonts! 1/2", "Damaged", 24.0, 0.0, Align::Center));
                let _ = render_layer(&words, &fonts, 1.0, ImageLimits::default());
                let _ = composite(frame(100, 100, WHITE), &Layers { layers: vec![words] }, &[], &fonts, 1.0, ImageLimits::default());
            }
        }
    }
}
