use super::*;

const WHITE: [u8; 4] = [255, 255, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn frame(width: u32, height: u32, background: [u8; 4]) -> Frame {
    Frame { width, height, background }
}

fn layer(id: &str, x: f64, y: f64, width: f64, height: f64, content: Content) -> Layer {
    Layer { id: id.into(), name: id.into(), visible: true, locked: false, opacity: 1.0, x, y, width, height, rotation: 0.0, flip_x: false, flip_y: false, content }
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
    composite(frame, &Layers { layers }, images, 1.0, ImageLimits::default()).unwrap()
}

#[test]
fn layers_stack_bottom_first_and_hidden_ones_are_skipped() {
    let out = draw(frame(200, 100, WHITE), vec![layer("a", 50.0, 50.0, 100.0, 100.0, rect("#ff0000")), layer("b", 100.0, 50.0, 100.0, 100.0, rect("#0000ff"))], &[]);
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
    let mut half = layer("a", 50.0, 50.0, 100.0, 100.0, Content::Shape { shape: ShapeKind::Rectangle, fill: Some("#000000".into()), stroke: Some("#000000".into()), stroke_width: 10.0 });
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
        let mut l = layer("img", 100.0, 100.0, 100.0, 100.0, Content::Image { input: 0 });
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
    let out = draw(frame(300, 100, WHITE), vec![layer("r", 50.0, 50.0, 100.0, 100.0, shape(ShapeKind::Rectangle)), layer("e", 150.0, 50.0, 100.0, 100.0, shape(ShapeKind::Ellipse)), layer("t", 250.0, 50.0, 100.0, 100.0, shape(ShapeKind::Triangle))], &[]);
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
    let (w, h) = text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, content.clone())).unwrap();
    assert!(w > 20.0 && w < 80.0 && h > 40.0 && h < 60.0, "{w} {h}");
    let ink = |out: &Raster, x0: u32, x1: u32| (x0..x1).any(|x| (0..out.height()).any(|y| at(out, x, y)[0] < 128));
    for (align, inked) in [(Align::Left, (true, false)), (Align::Right, (false, true))] {
        let out = draw(frame(400, 100, WHITE), vec![layer("t", 200.0, 50.0, 400.0, 100.0, text("Hi", 40.0, align))], &[]);
        assert_eq!((ink(&out, 0, 100), ink(&out, 300, 400)), inked, "{align:?}");
    }
    let two = text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, text("Hi\nHi", 40.0, Align::Left))).unwrap();
    assert!((two.1 - 2.0 * h).abs() <= 2.0, "{two:?}");
    // Tracking spreads the letters.
    let wide = styled("Hi", BUILTIN_FONT, 40.0, 1.0, Align::Left);
    assert!(text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, wide)).unwrap().0 > w + 30.0);
}

#[test]
fn an_unknown_font_falls_back_to_the_bundled_one() {
    let other = styled("Hi", "no such font", 40.0, 0.0, Align::Left);
    let builtin = text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, text("Hi", 40.0, Align::Left))).unwrap();
    assert_eq!(text_size(&layer("t", 0.0, 0.0, 1.0, 1.0, other)).unwrap(), builtin);
}

#[test]
fn a_preview_scales_the_whole_frame() {
    let layers = Layers { layers: vec![layer("a", 50.0, 50.0, 100.0, 100.0, rect("#ff0000"))] };
    let out = composite(frame(400, 200, WHITE), &layers, &[], 0.25, ImageLimits::default()).unwrap();
    assert_eq!((out.width(), out.height()), (100, 50));
    assert_eq!(at(&out, 10, 10), [255, 0, 0, 255]);
    assert_eq!(at(&out, 40, 10), WHITE);
}

#[test]
fn stored_layers_round_trip_and_hostile_ones_are_refused() {
    let mut l = layer("src-1", 10.0, 20.0, 30.0, 40.0, Content::Source { origin: "n1".into(), nth: 0 });
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
    assert_eq!(composite(frame(99, 100, WHITE), &layers, &[], 1.0, ImageLimits::default()).unwrap_err(), ImageError::Dimensions);
    assert_eq!(composite(frame(4097, 100, WHITE), &layers, &[], 1.0, ImageLimits::default()).unwrap_err(), ImageError::Dimensions);
    assert!(composite(frame(100, 100, WHITE), &layers, &[], 2.0, ImageLimits::default()).is_err());
    assert!(composite(frame(100, 100, WHITE), &layers, &[], f64::NAN, ImageLimits::default()).is_err());
    let missing = Layers { layers: vec![layer("a", 0.0, 0.0, 10.0, 10.0, Content::Image { input: 3 })] };
    assert_eq!(composite(frame(100, 100, WHITE), &missing, &[], 1.0, ImageLimits::default()).unwrap_err(), ImageError::Parameter("image layer"));
    let unresolved = Layers { layers: vec![layer("a", 0.0, 0.0, 10.0, 10.0, Content::Source { origin: "n".into(), nth: 0 })] };
    assert!(composite(frame(100, 100, WHITE), &unresolved, &[], 1.0, ImageLimits::default()).is_err());
    let busy = Layers { layers: (0..MAX_LAYERS).map(|i| layer(&format!("l{i}"), 0.0, 0.0, 1.0, 1.0, rect("#000000"))).collect() };
    let tight = ImageLimits { work: 100_000, ..ImageLimits::default() };
    assert_eq!(composite(frame(1000, 1000, WHITE), &busy, &[], 1.0, tight).unwrap_err(), ImageError::WorkLimit);
    let small = ImageLimits { working_bytes: 1_000_000, ..ImageLimits::default() };
    assert_eq!(composite(frame(1000, 1000, WHITE), &layers, &[], 1.0, small).unwrap_err(), ImageError::ScratchLimit);
}

#[test]
fn layers_far_off_the_frame_and_extreme_values_draw_nothing_and_do_not_panic() {
    let mut far = layer("a", MAX_COORD, -MAX_COORD, MAX_COORD, MAX_COORD, rect("#000000"));
    far.rotation = 3600.0;
    let mut thin = layer("b", 50.0, 50.0, f64::MIN_POSITIVE, MAX_COORD, Content::Shape { shape: ShapeKind::Triangle, fill: Some("#000000".into()), stroke: Some("#ff0000".into()), stroke_width: MAX_STROKE });
    thin.flip_y = true;
    let empty_text = layer("c", 50.0, 50.0, 10.0, 10.0, text("", 12.0, Align::Center));
    let out = draw(frame(100, 100, WHITE), vec![far, thin, empty_text, layer("d", 50.0, 50.0, 10.0, 10.0, text("\u{200d}\u{0301}🙂\r\n\t", MAX_TEXT_SIZE, Align::Right))], &[]);
    assert_eq!((out.width(), out.height()), (100, 100));
}
