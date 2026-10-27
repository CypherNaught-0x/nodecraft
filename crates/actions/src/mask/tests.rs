use super::*;

fn image(width: u32, height: u32, alpha: u8) -> Raster {
    let pixels = [10u8, 20, 30, alpha].repeat(width as usize * height as usize);
    Raster::new(width, height, pixels, ImageLimits::default()).unwrap()
}

fn value(mask: &Raster, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * mask.width() + x) * 4) as usize;
    mask.pixels()[at..at + 4].try_into().unwrap()
}

fn stroke(size: f64, hardness: f64, erase: bool, points: &[[f64; 2]]) -> Stroke {
    Stroke { size, hardness, erase, points: points.to_vec() }
}

#[test]
fn a_stroke_marks_white_and_transparent_and_leaves_the_rest_black_and_opaque() {
    let strokes = Strokes { strokes: vec![stroke(0.2, 1.0, false, &[[0.25, 0.5], [0.75, 0.5]])], over: None };
    let mask = render(&strokes, MaskOptions::default(), &image(100, 50, 255), ImageLimits::default()).unwrap();
    assert_eq!((mask.width(), mask.height()), (100, 50));
    // Along the stroke: edit. Far from it: keep.
    assert_eq!(value(&mask, 50, 25), [255, 255, 255, 0]);
    assert_eq!(value(&mask, 25, 25), [255, 255, 255, 0]);
    assert_eq!(value(&mask, 50, 2), [0, 0, 0, 255]);
    assert_eq!(value(&mask, 2, 25), [0, 0, 0, 255]);
    // The brush is 20% of the longer side (100 px): radius 10 px.
    assert_eq!(value(&mask, 50, 33), [255, 255, 255, 0]);
    assert_eq!(value(&mask, 50, 36), [0, 0, 0, 255]);
}

#[test]
fn soft_brushes_fade_and_erasing_cuts_back() {
    let soft = Strokes { strokes: vec![stroke(0.5, 0.0, false, &[[0.5, 0.5]])], over: None };
    let mask = render(&soft, MaskOptions::default(), &image(100, 100, 255), ImageLimits::default()).unwrap();
    let centre = value(&mask, 50, 50)[0];
    let middle = value(&mask, 62, 50)[0];
    let edge = value(&mask, 72, 50)[0];
    assert!(centre > middle && middle > edge && edge > 0, "{centre} {middle} {edge}");
    assert_eq!(value(&mask, 62, 50)[3], 255 - middle);

    let erased = Strokes { strokes: vec![stroke(0.5, 1.0, false, &[[0.5, 0.5]]), stroke(0.1, 1.0, true, &[[0.5, 0.5]])], over: None };
    let mask = render(&erased, MaskOptions::default(), &image(100, 100, 255), ImageLimits::default()).unwrap();
    assert_eq!(value(&mask, 50, 50), [0, 0, 0, 255]);
    assert_eq!(value(&mask, 70, 50), [255, 255, 255, 0]);
}

#[test]
fn transparent_areas_and_inversion() {
    let expanded = expand(&image(4, 4, 255), Margins { left: 2, top: 0, right: 0, bottom: 1 }, ImageLimits::default()).unwrap();
    assert_eq!((expanded.width(), expanded.height()), (6, 5));
    assert_eq!(value(&expanded, 0, 0), [0, 0, 0, 0]);
    assert_eq!(value(&expanded, 2, 0), [10, 20, 30, 255]);
    assert_eq!(value(&expanded, 5, 3), [10, 20, 30, 255]);
    assert_eq!(value(&expanded, 3, 4), [0, 0, 0, 0]);

    let margins = render(&Strokes::default(), MaskOptions { transparent: true, invert: false }, &expanded, ImageLimits::default()).unwrap();
    assert_eq!(value(&margins, 1, 1), [255, 255, 255, 0]);
    assert_eq!(value(&margins, 3, 1), [0, 0, 0, 255]);
    let inverted = render(&Strokes::default(), MaskOptions { transparent: true, invert: true }, &expanded, ImageLimits::default()).unwrap();
    assert_eq!(value(&inverted, 1, 1), [0, 0, 0, 255]);
    assert_eq!(value(&inverted, 3, 1), [255, 255, 255, 0]);
}

#[test]
fn strokes_round_trip_and_hostile_ones_are_refused() {
    let strokes = Strokes { strokes: vec![stroke(0.1, 0.5, true, &[[0.0, 0.0], [1.0, 1.0]]), stroke(0.1, 0.5, false, &[[0.5, 0.5]])], over: None };
    let json = strokes.encode().unwrap();
    assert!(!json.contains("\"erase\":false"), "{json}");
    assert_eq!(Strokes::parse(&json).unwrap(), strokes);
    for bad in [
        r#"{"strokes":[{"size":0,"hardness":0.5,"points":[[0.5,0.5]]}]}"#,
        r#"{"strokes":[{"size":2,"hardness":0.5,"points":[[0.5,0.5]]}]}"#,
        r#"{"strokes":[{"size":0.1,"hardness":1.5,"points":[[0.5,0.5]]}]}"#,
        r#"{"strokes":[{"size":0.1,"hardness":0.5,"points":[]}]}"#,
        r#"{"strokes":[{"size":0.1,"hardness":0.5,"points":[[5,0.5]]}]}"#,
        r#"{"strokes":[{"size":0.1,"hardness":0.5,"points":[[0.5,0.5]],"extra":1}]}"#,
        r#"{"strokes":"#,
    ] {
        assert!(Strokes::parse(bad).is_err(), "{bad}");
    }
    assert_eq!(Strokes::parse(&" ".repeat(MAX_ENCODED_BYTES + 1)), Err(ImageError::RequestLimit));
    let many = Strokes { strokes: vec![stroke(0.1, 0.5, false, &[[0.5, 0.5]]); MAX_STROKES + 1], over: None };
    assert!(many.check().is_err());
    let long = Strokes { strokes: vec![stroke(0.1, 0.5, false, &vec![[0.5, 0.5]; MAX_POINTS + 1])], over: None };
    assert!(long.check().is_err());
}

#[test]
fn work_and_size_are_bounded_before_anything_is_allocated() {
    let big = Strokes { strokes: vec![stroke(1.0, 0.5, false, &vec![[0.5, 0.5]; 4000])], over: None };
    let limits = ImageLimits { work: 1_000_000, ..ImageLimits::default() };
    assert_eq!(render(&big, MaskOptions::default(), &image(200, 200, 255), limits).unwrap_err(), ImageError::WorkLimit);
    assert!(Margins { left: MAX_MARGIN + 1, ..Margins::default() }.size(10, 10, ImageLimits::default()).is_err());
    let tight = ImageLimits { edge: 64, ..ImageLimits::default() };
    assert_eq!(expand(&image(60, 60, 255), Margins { right: 8, ..Margins::default() }, tight).unwrap_err(), ImageError::Dimensions);
}

#[test]
fn strokes_off_the_image_and_one_pixel_images_are_fine() {
    let strokes = Strokes { strokes: vec![stroke(MIN_SIZE, 0.0, false, &[[-1.0, -1.0], [2.0, 2.0]]), stroke(0.5, 1.0, false, &[[2.0, -1.0]])], over: None };
    let mask = render(&strokes, MaskOptions::default(), &image(1, 1, 255), ImageLimits::default()).unwrap();
    assert_eq!(value(&mask, 0, 0), [255, 255, 255, 0]);
    assert!(!Strokes { strokes: vec![stroke(0.1, 0.5, true, &[[0.5, 0.5]])], over: None }.paints());
}

#[test]
fn the_painted_image_round_trips_and_older_masks_read_without_it() {
    let over = Painted { hash: "ab".repeat(32), width: 1024, height: 768 };
    let strokes = Strokes { strokes: vec![stroke(0.1, 0.5, false, &[[0.5, 0.5]])], over: Some(over.clone()) };
    assert_eq!(Strokes::parse(&strokes.encode().unwrap()).unwrap(), strokes);
    let older = Strokes::parse(r#"{"strokes":[{"size":0.1,"hardness":0.5,"points":[[0.5,0.5]]}]}"#).unwrap();
    assert_eq!(older.over, None);
    assert!(!older.encode().unwrap().contains("over"), "an unbound mask stores as before");
    for bad in [
        Painted { hash: "AB".repeat(32), ..over.clone() },
        Painted { hash: "ab".repeat(31), ..over.clone() },
        Painted { width: 0, ..over.clone() },
        Painted { height: 0, ..over.clone() },
    ] {
        assert_eq!(Strokes { strokes: vec![], over: Some(bad) }.check(), Err(ImageError::Parameter("mask.over")));
    }
    assert!(Strokes::parse(r#"{"strokes":[],"over":{"hash":"x","width":1,"height":1,"extra":1}}"#).is_err());
}

#[test]
fn only_a_different_aspect_ratio_is_a_reshape() {
    let over = Painted { hash: "00".repeat(32), width: 1024, height: 1024 };
    assert!(!over.reshaped(1024, 1024));
    assert!(!over.reshaped(2048, 2048), "a larger copy keeps the shape");
    assert!(!over.reshaped(1024, 1020), "rounding is not a reshape");
    assert!(over.reshaped(1536, 1024));
    assert!(over.reshaped(1024, 1536));
    assert!(!Painted { width: u32::MAX, height: u32::MAX, ..over }.reshaped(1, 1));
}

#[test]
fn a_controlled_mask_admits_work_and_stops_between_rows() {
    let strokes = Strokes { strokes: vec![stroke(1.0, 1.0, false, &[[0.0, 0.0], [1.0, 1.0]])], over: None };
    let total = std::cell::Cell::new(0u64);
    let checks = std::cell::Cell::new(0usize);
    let result = render_controlled(&strokes, MaskOptions::default(), &image(256, 256, 255), ImageLimits::default(), &|units| {
        total.set(total.get().saturating_add(units));
        checks.set(checks.get() + 1);
        checks.get() >= 4
    });
    assert!(matches!(result, Err(ImageError::Cancelled)));
    assert!(total.get() > 256 * 256 && checks.get() >= 4);
    assert!(matches!(expand_controlled(&image(8, 8, 255), Margins::default(), ImageLimits::default(), &|units| units > 0), Err(ImageError::Cancelled)));
}
