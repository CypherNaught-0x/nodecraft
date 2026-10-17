use super::params::*;
use super::*;
fn l() -> ImageLimits {
    ImageLimits::default()
}
fn raster(w: u32, h: u32, p: &[[u8; 4]]) -> Raster {
    Raster::new(w, h, p.iter().flatten().copied().collect(), l()).unwrap()
}
fn image(req: ImageRequest, source: &Raster) -> Raster {
    let ImageResult::Image(out) = req.execute(ImageInputs::single(source), &mut CpuExecutor::serial(), l()).unwrap() else { panic!("single image") };
    out
}
fn flat(w: u32, h: u32, c: [u8; 4]) -> Raster {
    raster(w, h, &vec![c; (w * h) as usize])
}
fn close(a: &[u8], b: &[u8], t: u8) {
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b) {
        assert!(x.abs_diff(*y) <= t, "{a:?} != {b:?}")
    }
}
fn req(b: Blur) -> ImageRequest {
    ImageRequest::Blur { options: b }
}
fn blurs() -> Vec<Blur> {
    vec![
        Blur::Gaussian { sigma: 2. },
        Blur::Box { radius: 2 },
        Blur::Motion { length: 8., angle: 20., samples: 17 },
        Blur::Radial { mode: RadialMode::Spin, center: Point::default(), amount: 0.2, samples: 17 },
        Blur::Radial { mode: RadialMode::Zoom, center: Point::default(), amount: 0.2, samples: 17 },
        Blur::Bilateral { radius: 3, spatial_sigma: 2., range_sigma: 0.1 },
        Blur::Bokeh { radius: 4., samples: 25, aperture: Aperture::Disk },
        Blur::Bokeh { radius: 4., samples: 25, aperture: Aperture::Hexagon },
        Blur::TiltShift { sigma: 4., focus: Point::default(), angle: 25., band_width: 0.25, falloff: 0.1 },
        Blur::TargetColor { sigma: 2., matcher: Matcher::default(), selection: Selection::Matched },
    ]
}
#[test]
fn exact_paths_preserve_hidden_and_low_alpha_bytes() {
    let src = raster(2, 2, &[[245, 7, 31, 0], [3, 97, 230, 1], [72, 1, 8, 7], [255, 17, 191, 255]]);
    for request in [
        ImageRequest::Grade { options: Grade::default() },
        ImageRequest::Filter { options: Filter::default() },
        ImageRequest::Tint { options: Tint::default() },
        ImageRequest::Crop { options: Crop::Full {} },
        ImageRequest::Resize { options: Resize::default() },
        ImageRequest::Aspect { options: Aspect::default() },
        ImageRequest::Rotate { options: Rotate::default() },
    ] {
        assert_eq!(image(request, &src).pixels(), src.pixels());
    }
    let flipped = image(ImageRequest::Flip { axis: Axis::Horizontal }, &src);
    assert_eq!(flipped.pixels(), &[3, 97, 230, 1, 245, 7, 31, 0, 255, 17, 191, 255, 72, 1, 8, 7]);
    assert_eq!(image(ImageRequest::Flip { axis: Axis::Horizontal }, &flipped).pixels(), src.pixels());
    let crop = image(ImageRequest::Crop { options: Crop::Pixels { x: 0, y: 0, width: 1, height: 1 } }, &src);
    assert_eq!(crop.pixels(), &[245, 7, 31, 0]);
    let rotated = image(ImageRequest::Rotate { options: Rotate { angle: 90., ..Rotate::default() } }, &src);
    assert_eq!(rotated.pixels(), &[72, 1, 8, 7, 245, 7, 31, 0, 255, 17, 191, 255, 3, 97, 230, 1]);
    let resized = image(ImageRequest::Resize { options: Resize { size: ResizeSize::Scale { factor: 2. }, resampling: Resampling::Nearest } }, &src);
    assert_eq!(resized.pixels().get(..4).unwrap(), &[245, 7, 31, 0]);
}
#[test]
fn grade_presets_tint_key_and_replacement_have_analytic_semantics() {
    let white = flat(1, 1, [255, 255, 255, 127]);
    assert_eq!(image(ImageRequest::Grade { options: Grade { brightness: -1., ..Grade::default() } }, &white).pixels(), &[188, 188, 188, 127]);
    for preset in [Preset::Grayscale {}, Preset::Sepia {}, Preset::Noir {}, Preset::Duotone { shadow: Color::BLACK, highlight: Color::WHITE }] {
        let out = image(ImageRequest::Filter { options: Filter { preset, intensity: 1. } }, &white);
        assert_eq!(out.pixels().last(), Some(&127));
    }
    let src = flat(1, 1, [188, 188, 188, 200]);
    for blend in [Blend::Normal, Blend::Multiply, Blend::Screen, Blend::Overlay, Blend::Darken, Blend::Lighten, Blend::Difference] {
        let out = image(ImageRequest::Tint { options: Tint { color: Color::BLACK, intensity: 1., blend } }, &src);
        let expected = match blend {
            Blend::Normal | Blend::Multiply | Blend::Darken => 0,
            Blend::Overlay => 23,
            _ => 188,
        };
        if blend == Blend::Overlay {
            assert!(out.pixels().first().copied().unwrap() < 30)
        } else {
            assert_eq!(out.pixels(), &[expected, expected, expected, 200]);
        }
    }
    let green = flat(1, 1, [0, 255, 0, 128]);
    assert_eq!(
        image(
            ImageRequest::Key { options: Key { matcher: Matcher { tolerance: 0., softness: 0., ..Matcher::default() }, mode: KeyMode::RemoveMatched } },
            &green
        )
        .pixels(),
        &[0; 4]
    );
    let replacement = Color { r: 255, g: 0, b: 0, a: 64 };
    for replace_alpha in [false, true] {
        let out = image(ImageRequest::ReplaceColor { options: ReplaceColor { matcher: Matcher::default(), replacement, replace_alpha } }, &green);
        assert_eq!(out.pixels(), &[255, 0, 0, if replace_alpha { 64 } else { 128 }]);
    }
    let key_all =
        image(ImageRequest::Key { options: Key { matcher: Matcher { tolerance: 1., softness: 0., ..Matcher::default() }, mode: KeyMode::KeepMatched } }, &src);
    assert_eq!(key_all.pixels(), src.pixels());
}
#[test]
fn backgrounds_apply_through_transparency_even_on_shortcuts() {
    let src = flat(2, 1, [255, 0, 0, 128]);
    let white = Color::WHITE;
    for angle in [0., 90., 180., 270.] {
        let out = image(ImageRequest::Rotate { options: Rotate { angle, background: white, ..Rotate::default() } }, &src);
        for p in out.pixels().as_chunks::<4>().0 {
            close(p, &[255, 187, 187, 255], 1)
        }
    }
    let out = image(ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Solid { color: white } }, ..Aspect::default() } }, &src);
    for p in out.pixels().as_chunks::<4>().0 {
        close(p, &[255, 187, 187, 255], 1)
    }
    let out = image(ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { sigma: 0. } }, ..Aspect::default() } }, &src);
    for p in out.pixels().as_chunks::<4>().0 {
        close(p, &[255, 0, 0, 192], 1)
    }
}
#[test]
fn overlay_source_over_clips_and_preserves_role_order() {
    let base = flat(1, 1, [0, 0, 255, 255]);
    let top = flat(1, 1, [255, 0, 0, 128]);
    let request = ImageRequest::Overlay { options: Overlay { size: OverlaySize::Source {}, ..Overlay::default() } };
    let ImageResult::Image(out) = request.execute(ImageInputs { source: &base, overlay: Some(&top) }, &mut CpuExecutor::serial(), l()).unwrap() else {
        panic!()
    };
    close(out.pixels(), &[188, 0, 187, 255], 1);
    assert_eq!(request.execute(ImageInputs::single(&base), &mut CpuExecutor::serial(), l()).unwrap_err(), ImageError::InputRoles);
    let request = ImageRequest::Overlay { options: Overlay { offset_x: 20, size: OverlaySize::Source {}, ..Overlay::default() } };
    let ImageResult::Image(out) = request.execute(ImageInputs { source: &base, overlay: Some(&top) }, &mut CpuExecutor::serial(), l()).unwrap() else {
        panic!()
    };
    assert_eq!(out.pixels(), base.pixels());
}
#[test]
fn mixed_axis_area_resize_and_premultiplied_edges() {
    let src = raster(2, 1, &[[255, 0, 0, 255], [0, 0, 255, 0]]);
    let out = image(
        ImageRequest::Resize {
            options: Resize { size: ResizeSize::Exact { width: 1, height: 2, fit: Fit::Stretch, anchor: Point::default() }, resampling: Resampling::Area },
        },
        &src,
    );
    assert_eq!((out.width(), out.height()), (1, 2));
    for p in out.pixels().as_chunks::<4>().0 {
        close(p, &[255, 0, 0, 128], 1)
    }
    let vertical = raster(1, 2, &[[255, 0, 0, 255], [0, 0, 255, 0]]);
    let out = image(
        ImageRequest::Resize {
            options: Resize { size: ResizeSize::Exact { width: 2, height: 1, fit: Fit::Stretch, anchor: Point::default() }, resampling: Resampling::Area },
        },
        &vertical,
    );
    for p in out.pixels().as_chunks::<4>().0 {
        close(p, &[255, 0, 0, 128], 1)
    }
}
#[test]
fn every_blur_preserves_constant_colors_and_alpha_without_fringe() {
    let src = flat(5, 4, [23, 87, 190, 73]);
    for blur in blurs() {
        let out = image(req(blur.clone()), &src);
        close(out.pixels(), src.pixels(), 1);
    }
    let src = raster(3, 1, &[[255, 0, 0, 255], [0, 0, 255, 0], [0, 0, 255, 0]]);
    for blur in blurs() {
        let out = image(req(blur), &src);
        for p in out.pixels().as_chunks::<4>().0 {
            if p.get(3).copied().unwrap() > 0 {
                assert_eq!(p.get(2), Some(&0));
            }
        }
    }
}
#[test]
fn box_matches_independent_square_oracle_at_edges() {
    let src = raster(3, 2, &[[0, 0, 0, 255], [255, 255, 255, 255], [0, 0, 0, 255], [255, 255, 255, 255], [0, 0, 0, 255], [255, 255, 255, 255]]);
    let out = image(req(Blur::Box { radius: 1 }), &src);
    for y in 0i32..2 {
        for x in 0i32..3 {
            let mut count = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let ix = (x + dx).clamp(0, 2);
                    let iy = (y + dy).clamp(0, 1);
                    if src.pixels().get(((iy * 3 + ix) * 4) as usize) == Some(&255) {
                        count += 1
                    }
                }
            }
            let v = f64::from(count) / 9.;
            let expected = ((1.055 * v.powf(1. / 2.4) - 0.055) * 255.).round() as u8;
            let offset = ((y * 3 + x) * 4) as usize;
            close(out.pixels().get(offset..offset + 4).unwrap(), &[expected, expected, expected, 255], 1)
        }
    }
}
#[test]
fn bilateral_preserves_hard_edges_and_tilt_shift_focus() {
    let src = raster(3, 1, &[[0, 0, 0, 255], [255, 255, 255, 255], [0, 0, 0, 255]]);
    let bilateral = image(req(Blur::Bilateral { radius: 1, spatial_sigma: 1., range_sigma: 1. / 65536. }), &src);
    assert_eq!(bilateral.pixels(), src.pixels());
    let focused = image(req(Blur::TiltShift { sigma: 2., focus: Point::default(), angle: 0., band_width: 1., falloff: 0.1 }), &src);
    assert_eq!(focused.pixels(), src.pixels());
}
#[test]
fn tiny_positive_parameters_and_nonfinite_values_reject_before_allocation() {
    let src = flat(1, 1, [0, 0, 0, 255]);
    for sigma in [f64::from_bits(1), f64::MIN_POSITIVE, (1f64 / 256.).next_down(), f64::NAN, f64::INFINITY] {
        let before = super::raster::allocations();
        assert!(req(Blur::Gaussian { sigma }).execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), l()).is_err());
        assert_eq!(super::raster::allocations(), before)
    }
    assert!(req(Blur::Gaussian { sigma: 1. / 256. }).execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), l()).is_ok());
    for falloff in [0., f64::from_bits(1), (1f64 / 65536.).next_down()] {
        assert!(
            req(Blur::TiltShift { sigma: 0., focus: Point::default(), angle: 0., band_width: 0., falloff })
                .execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), l())
                .is_err()
        )
    }
    assert!(
        req(Blur::Bilateral { radius: 0, spatial_sigma: f64::from_bits(1), range_sigma: 0.1 })
            .execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), l())
            .is_err()
    );
}
#[test]
fn exact_work_caps_and_capacity_errors_precede_output_allocation() {
    let src = flat(2, 2, [0, 0, 0, 255]);
    let request = ImageRequest::Resize { options: Resize { size: ResizeSize::Scale { factor: 2. }, resampling: Resampling::Bilinear } };
    let before = super::raster::allocations();
    assert_eq!(request.execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), ImageLimits { work: 63, ..l() }).unwrap_err(), ImageError::WorkLimit);
    assert_eq!(super::raster::allocations(), before);
    assert!(request.execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), ImageLimits { work: 64, ..l() }).is_ok());
    let request = ImageRequest::Overlay { options: Overlay { size: OverlaySize::Source {}, ..Overlay::default() } };
    let inputs = ImageInputs { source: &src, overlay: Some(&src) };
    assert_eq!(request.execute(inputs, &mut CpuExecutor::serial(), ImageLimits { work: 19, ..l() }).unwrap_err(), ImageError::WorkLimit);
    assert!(request.execute(inputs, &mut CpuExecutor::serial(), ImageLimits { work: 20, ..l() }).is_ok());
    let before = super::raster::allocations();
    let duplicate = ImageRequest::Duplicate { count: 3 };
    assert_eq!(
        duplicate.execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), ImageLimits { output_bytes: 47, ..l() }).unwrap_err(),
        ImageError::OutputLimit
    );
    assert_eq!(super::raster::allocations(), before);
    let ImageResult::Images(copies) =
        duplicate.execute(ImageInputs::single(&src), &mut CpuExecutor::serial(), ImageLimits { output_bytes: 48, ..l() }).unwrap()
    else {
        panic!()
    };
    assert_eq!(copies.len(), 3);
    assert!(copies.iter().all(|p| std::ptr::eq(p.pixels().as_ptr(), src.pixels().as_ptr())));
    assert!(Raster::new(u32::MAX, u32::MAX, vec![], l()).is_err());
    let spare = Vec::with_capacity(64);
    assert!(Raster::new(1, 1, spare, l()).is_err());
}
#[test]
fn serde_all_variants_and_required_controls_round_trip() {
    let mut requests = vec![
        ImageRequest::Grade { options: Grade::default() },
        ImageRequest::Filter { options: Filter::default() },
        ImageRequest::Tint { options: Tint::default() },
        ImageRequest::Key { options: Key::default() },
        ImageRequest::ReplaceColor { options: ReplaceColor::default() },
        ImageRequest::Aspect { options: Aspect::default() },
        ImageRequest::Rotate { options: Rotate::default() },
        ImageRequest::Flip { axis: Axis::Both },
        ImageRequest::Crop { options: Crop::Full {} },
        ImageRequest::Resize { options: Resize::default() },
        ImageRequest::Overlay { options: Overlay::default() },
        ImageRequest::Duplicate { count: 2 },
    ];
    requests.extend(blurs().into_iter().map(req));
    for request in requests {
        let wire = serde_json::to_value(&request).unwrap();
        let round: ImageRequest = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(round, request);
        let mut unknown = wire.clone();
        unknown["unknown"] = true.into();
        assert!(serde_json::from_value::<ImageRequest>(unknown).is_err());
    }
    for wire in [
        r#"{"action":"image.duplicate"}"#,
        r#"{"action":"image.duplicate","count":null}"#,
        r#"{"action":"image.blur","options":{"kind":"gaussian"}}"#,
        r#"{"action":"image.blur","options":{"kind":"gaussian","sigma":2,"hidden":4}}"#,
    ] {
        assert!(serde_json::from_str::<ImageRequest>(wire).is_err())
    }
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn parallel_and_serial_are_identical() {
    let src = raster(3, 2, &[[255, 0, 0, 128], [0, 255, 0, 255], [0, 0, 255, 0], [17, 91, 37, 20], [243, 1, 9, 254], [71, 83, 90, 255]]);
    let mut exec = CpuExecutor::parallel(2).unwrap();
    for blur in blurs() {
        let request = req(blur);
        let expected = image(request.clone(), &src);
        let ImageResult::Image(out) = request.execute(ImageInputs::single(&src), &mut exec, l()).unwrap() else { panic!() };
        assert_eq!(out.pixels(), expected.pixels());
    }
}

#[test]
fn exact_rational_aspects_do_not_lose_or_grow_rows() {
    for (w, h) in [(1, 93), (1, 49), (93, 1), (49, 1), (7, 13)] {
        let src = flat(w, h, [31, 79, 231, 255]);
        for ratio in [Ratio::Source {}, Ratio::Custom { numerator: w, denominator: h }] {
            for mode in [AspectMode::Crop {}, AspectMode::Pad { padding: Padding::Solid { color: Color::TRANSPARENT } }] {
                let out = image(ImageRequest::Aspect { options: Aspect { ratio, mode, anchor: Point { x: 1., y: 0. } } }, &src);
                assert_eq!((out.width(), out.height()), (w, h));
                assert_eq!(out.pixels(), src.pixels());
            }
        }
    }
}
#[test]
fn copy_normalization_reserves_source_copy_and_header_before_allocation() {
    let source = [1, 2, 3, 4];
    let before = super::raster::allocations();
    for working_bytes in [0, 71] {
        assert!(matches!(Raster::copy_from(1, 1, &source, ImageLimits { working_bytes, ..l() }), Err(ImageError::ScratchLimit)));
        assert_eq!(super::raster::allocations(), before)
    }
    assert!(Raster::copy_from(1, 1, &source, ImageLimits { working_bytes: 72, ..l() }).is_ok());
}
#[test]
fn computed_alpha_rounding_to_zero_canonicalizes_rgb() {
    let src = flat(1, 1, [255, 0, 0, 1]);
    let out = image(
        ImageRequest::Key { options: Key { matcher: Matcher { target: Color::BLACK, tolerance: 0., softness: 0.6 }, mode: KeyMode::RemoveMatched } },
        &src,
    );
    // d=sqrt(1/3), so removal retains almost all alpha: use keep to produce the tiny remainder.
    let tiny =
        image(ImageRequest::Key { options: Key { matcher: Matcher { target: Color::BLACK, tolerance: 0., softness: 0.6 }, mode: KeyMode::KeepMatched } }, &src);
    assert_eq!(tiny.pixels(), &[0; 4]);
    assert_eq!(out.pixels().last(), Some(&1));
}

fn png(image: nodecraft_media::image::DynamicImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, nodecraft_media::image::ImageFormat::Png).unwrap();
    out.into_inner()
}
#[test]
fn bounded_wire_and_borrowed_adapters_reject_before_normalization() {
    assert_eq!(ImageRequest::from_json_bytes(&vec![b' '; 65_537]).unwrap_err(), ImageError::RequestLimit);
    assert_eq!(ImageRequest::from_json_bytes(br#"{"action":"image.duplicate","count":null}"#).unwrap_err(), ImageError::RequestSyntax);
    let data = [255, 0, 0, 255];
    let view = RasterView::new(1, 1, &data, l()).unwrap();
    let request = ImageRequest::Resize { options: Resize { size: ResizeSize::Scale { factor: 2. }, resampling: Resampling::Bilinear } };
    let before = super::raster::allocations();
    assert_eq!(request.execute_views(view, None, &mut CpuExecutor::serial(), ImageLimits { work: 15, ..l() }).unwrap_err(), ImageError::WorkLimit);
    assert_eq!(super::raster::allocations(), before);
    let ImageResult::Image(out) = request.execute_views(view, None, &mut CpuExecutor::serial(), l()).unwrap() else { panic!() };
    assert_eq!(out.pixels(), &data.repeat(4));
    let decoded = nodecraft_media::image::DynamicImage::ImageRgb8(nodecraft_media::image::RgbImage::from_raw(1, 1, vec![255, 0, 0]).unwrap());
    let ImageResult::Image(out) = request.execute_decoded(&decoded, None, &mut CpuExecutor::serial(), l()).unwrap() else { panic!() };
    assert_eq!(out.pixels(), &data.repeat(4));
    let mut spare = Vec::with_capacity(128);
    spare.extend_from_slice(&[255, 0, 0]);
    let decoded = nodecraft_media::image::DynamicImage::ImageRgb8(nodecraft_media::image::RgbImage::from_raw(1, 1, spare).unwrap());
    let before = super::raster::allocations();
    assert_eq!(
        request.execute_decoded(&decoded, None, &mut CpuExecutor::serial(), ImageLimits { input_bytes: 127, ..l() }).unwrap_err(),
        ImageError::InputLimit
    );
    assert_eq!(super::raster::allocations(), before);
}
#[test]
fn encoded_probe_enforces_action_limits_and_safe_error_categories() {
    use nodecraft_media::image::{DynamicImage, ImageBuffer, Luma, Rgba};
    let bytes = png(DynamicImage::ImageRgba8(ImageBuffer::from_pixel(2, 2, Rgba([12, 43, 87, 255]))));
    let request = ImageRequest::Grade { options: Grade::default() };
    let ImageResult::Image(out) = request.execute_encoded(&bytes, None, &mut CpuExecutor::serial(), l()).unwrap() else { panic!() };
    assert_eq!(out.pixels(), &[12, 43, 87, 255].repeat(4));
    let sixteen = png(DynamicImage::ImageLuma16(ImageBuffer::from_pixel(1, 1, Luma([32768]))));
    let ImageResult::Image(out) = request.execute_encoded(&sixteen, None, &mut CpuExecutor::serial(), l()).unwrap() else { panic!() };
    assert_eq!(out.pixels(), &[128, 128, 128, 255]);
    let before = super::raster::allocations();
    assert_eq!(request.execute_encoded(&bytes, None, &mut CpuExecutor::serial(), ImageLimits { work: 3, ..l() }).unwrap_err(), ImageError::WorkLimit);
    assert_eq!(super::raster::allocations(), before);
    assert_eq!(request.execute_encoded(&bytes, None, &mut CpuExecutor::serial(), ImageLimits { pixels: 3, ..l() }).unwrap_err(), ImageError::Dimensions);
    assert_eq!(request.execute_encoded(&bytes, None, &mut CpuExecutor::serial(), ImageLimits { encoded_bytes: 1, ..l() }).unwrap_err(), ImageError::InputLimit);
    assert_eq!(request.execute_encoded(b"%PDF-1.7", None, &mut CpuExecutor::serial(), l()).unwrap_err(), ImageError::UnsupportedFormat);
    // Valid tiny PNG header alone, admitted by the decoder before pixel body access.
    let header = bytes.get(..33).unwrap();
    assert_eq!(request.execute_encoded(header, None, &mut CpuExecutor::serial(), ImageLimits { edge: 1, ..l() }).unwrap_err(), ImageError::DecodeLimit);
    let raw = DynamicImage::ImageRgba16(ImageBuffer::from_pixel(2, 2, Rgba([1, 2, 3, 4])));
    let before = super::raster::allocations();
    assert_eq!(
        request.execute_decoded(&raw, None, &mut CpuExecutor::serial(), ImageLimits { working_bytes: 4140, ..l() }).unwrap_err(),
        ImageError::ScratchLimit
    );
    assert_eq!(super::raster::allocations(), before);
}
#[test]
fn default_four_k_blur_admission_fits_work_and_scratch() {
    for b in blurs() {
        let request = req(b);
        let plan = super::geometry::layout(&request, 3840, 2160, None).unwrap();
        assert!(super::validate::admit(&request, &plan, 3840 * 2160 * 4, l()).is_ok(), "{request:?}");
    }
}
#[test]
fn gaussian_matches_a_direct_two_dimensional_impulse_oracle() {
    let src = raster(
        3,
        3,
        &[[0, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255], [255, 255, 255, 255], [0, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255]],
    );
    let out = image(req(Blur::Gaussian { sigma: 1. }), &src);
    for y in 0i32..3 {
        for x in 0i32..3 {
            let mut light = 0.;
            let mut norm = 0.;
            for dy in -3i32..=3 {
                for dx in -3i32..=3 {
                    let weight = (-0.5 * f64::from(dx * dx + dy * dy)).exp();
                    norm += weight;
                    if (x + dx).clamp(0, 2) == 1 && (y + dy).clamp(0, 2) == 1 {
                        light += weight
                    }
                }
            }
            let v = light / norm;
            let expected = if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1. / 2.4) - 0.055 };
            let offset = ((y * 3 + x) * 4) as usize;
            close(
                out.pixels().get(offset..offset + 4).unwrap(),
                &[(expected * 255.).round() as u8, (expected * 255.).round() as u8, (expected * 255.).round() as u8, 255],
                1,
            )
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]
    #[test]
    fn exact_transforms_preserve_original_bytes(w in 1u32..8,h in 1u32..8,seed in proptest::num::u64::ANY){
        let pixels:Vec<[u8;4]>=(0..w*h).map(|i|{let n=seed.wrapping_add(u64::from(i).wrapping_mul(0x9e3779b9));[n as u8,(n>>8)as u8,(n>>16)as u8,(n>>24)as u8]}).collect();
        let source=raster(w,h,&pixels);let a=image(ImageRequest::Flip{axis:Axis::Both},&source);let a=image(ImageRequest::Flip{axis:Axis::Both},&a);proptest::prop_assert_eq!(a.pixels(),source.pixels());
        let mut rotated=source.clone();for _ in 0..4{rotated=image(ImageRequest::Rotate{options:Rotate{angle:90.,..Rotate::default()}},&rotated)}proptest::prop_assert_eq!(rotated.pixels(),source.pixels());
        let blurred=image(req(Blur::Gaussian{sigma:1.}),&source);for p in blurred.pixels().as_chunks::<4>().0{if p.last()==Some(&0){proptest::prop_assert_eq!(p,&[0;4]);}}
    }
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "writes an original public-safe visual review artifact"]
fn write_contact_sheet() {
    use nodecraft_media::image::{DynamicImage, RgbaImage};
    let mut source = Vec::new();
    for y in 0u32..64 {
        for x in 0u32..64 {
            let checker = if (x / 8 + y / 8) % 2 == 0 { 210 } else { 40 };
            source.extend_from_slice(&[(x * 4) as u8, checker, (y * 4) as u8, if x < 12 { (x * 21) as u8 } else { 255 }]);
        }
    }
    let source = Raster::new(64, 64, source, l()).unwrap();
    let overlay = flat(16, 16, [255, 255, 255, 128]);
    let mut cases = vec![
        ImageRequest::Grade { options: Grade::default() },
        ImageRequest::Grade { options: Grade { warmth: 0.8, contrast: 1.3, saturation: 0.5, brightness: 0.2 } },
        ImageRequest::Filter { options: Filter { preset: Preset::Grayscale {}, intensity: 1. } },
        ImageRequest::Filter { options: Filter { preset: Preset::Sepia {}, intensity: 1. } },
        ImageRequest::Filter {
            options: Filter {
                preset: Preset::Duotone { shadow: Color { r: 12, g: 0, b: 80, a: 255 }, highlight: Color { r: 255, g: 210, b: 32, a: 255 } },
                intensity: 1.,
            },
        },
        ImageRequest::Filter { options: Filter { preset: Preset::Noir {}, intensity: 1. } },
        ImageRequest::Tint { options: Tint { color: Color { r: 255, g: 80, b: 0, a: 255 }, intensity: 0.7, blend: Blend::Screen } },
        ImageRequest::Key {
            options: Key {
                matcher: Matcher { target: Color { r: 128, g: 210, b: 128, a: 255 }, tolerance: 0.25, softness: 0.2 },
                mode: KeyMode::RemoveMatched,
            },
        },
        ImageRequest::ReplaceColor {
            options: ReplaceColor {
                matcher: Matcher { tolerance: 0.4, ..Matcher::default() },
                replacement: Color { r: 230, g: 30, b: 100, a: 255 },
                replace_alpha: false,
            },
        },
        ImageRequest::Aspect {
            options: Aspect { ratio: Ratio::Landscape16_9 {}, mode: AspectMode::Pad { padding: Padding::Blurred { sigma: 4. } }, anchor: Point::default() },
        },
        ImageRequest::Rotate { options: Rotate { angle: 25., background: Color::WHITE, ..Rotate::default() } },
        ImageRequest::Flip { axis: Axis::Horizontal },
        ImageRequest::Crop { options: Crop::Pixels { x: 8, y: 8, width: 40, height: 40 } },
        ImageRequest::Resize {
            options: Resize { size: ResizeSize::Exact { width: 32, height: 64, fit: Fit::Stretch, anchor: Point::default() }, resampling: Resampling::Area },
        },
        ImageRequest::Overlay { options: Overlay { size: OverlaySize::Source {}, ..Overlay::default() } },
    ];
    cases.extend(blurs().into_iter().map(|b| {
        req(match b {
            Blur::Radial { mode, center, samples, .. } => Blur::Radial { mode, center, samples, amount: 0.8 },
            other => other,
        })
    }));
    let count = cases.len();
    let cols = 5usize;
    let rows = count.div_ceil(cols);
    let mut sheet = vec![0u8; cols * 64 * rows * 64 * 4];
    let mut legend = String::new();
    for (i, request) in cases.into_iter().enumerate() {
        let inputs = ImageInputs { source: &source, overlay: if matches!(request, ImageRequest::Overlay { .. }) { Some(&overlay) } else { None } };
        let ImageResult::Image(out) = request.execute(inputs, &mut CpuExecutor::serial(), l()).unwrap() else { panic!() };
        let thumb = image(
            ImageRequest::Resize {
                options: Resize {
                    size: ResizeSize::Exact { width: 64, height: 64, fit: Fit::Stretch, anchor: Point::default() },
                    resampling: Resampling::Bilinear,
                },
            },
            &out,
        );
        legend.push_str(&format!("{}: {:?}\n", i + 1, request));
        for y in 0..64usize {
            let row = thumb.pixels().get(y * 256..(y + 1) * 256).unwrap();
            let offset = ((i / cols * 64 + y) * cols * 64 + i % cols * 64) * 4;
            sheet.get_mut(offset..offset + 256).unwrap().copy_from_slice(row);
        }
    }
    let png = png(DynamicImage::ImageRgba8(RgbaImage::from_raw((cols * 64) as u32, (rows * 64) as u32, sheet).unwrap()));
    std::fs::write("/tmp/nodecraft-image85-contact.png", png).unwrap();
    std::fs::write("/tmp/nodecraft-image85-contact.txt", legend).unwrap();
}

#[test]
fn original_wire_fixtures_round_trip_and_execute() {
    #[derive(serde::Deserialize)]
    struct Fixture {
        name: String,
        request: serde_json::Value,
        source: [u8; 4],
        expected: [u8; 4],
    }
    let fixtures: Vec<Fixture> = serde_json::from_str(include_str!("../../tests/fixtures/image_requests.json")).unwrap();
    for fixture in fixtures {
        let bytes = serde_json::to_vec(&fixture.request).unwrap();
        let request = ImageRequest::from_json_bytes(&bytes).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), fixture.request, "{}", fixture.name);
        let src = flat(1, 1, fixture.source);
        assert_eq!(image(request, &src).pixels(), &fixture.expected, "{}", fixture.name);
    }
}

#[test]
fn decoded_normalized_aggregate_rejects_before_either_conversion() {
    use nodecraft_media::image::{DynamicImage, GrayImage};
    let gray = DynamicImage::ImageLuma8(GrayImage::from_raw(1, 1, vec![32]).unwrap());
    let before = super::raster::allocations();
    let request = ImageRequest::Grade { options: Grade::default() };
    assert_eq!(request.execute_decoded(&gray, None, &mut CpuExecutor::serial(), ImageLimits { input_bytes: 1, ..l() }).unwrap_err(), ImageError::InputLimit);
    assert_eq!(super::raster::allocations(), before);
    let overlay = ImageRequest::Overlay { options: Overlay::default() };
    assert_eq!(
        overlay.execute_decoded(&gray, Some(&gray), &mut CpuExecutor::serial(), ImageLimits { input_bytes: 7, ..l() }).unwrap_err(),
        ImageError::InputLimit
    );
    assert_eq!(super::raster::allocations(), before);
    let bytes = png(gray);
    assert_eq!(overlay.execute_encoded(&bytes, Some(b"%PDF-1.7"), &mut CpuExecutor::serial(), l()).unwrap_err(), ImageError::UnsupportedFormat);
    let large = png(nodecraft_media::image::DynamicImage::ImageLuma8(GrayImage::from_pixel(2, 2, nodecraft_media::image::Luma([12]))));
    assert_eq!(
        overlay.execute_encoded(&bytes, large.get(..33), &mut CpuExecutor::serial(), ImageLimits { edge: 1, ..l() }).unwrap_err(),
        ImageError::DecodeLimit
    );
}

fn rgba_at(image: &Raster, x: u32, y: u32) -> &[u8] {
    let offset = ((y * image.width() + x) * 4) as usize;
    image.pixels().get(offset..offset + 4).unwrap()
}
#[test]
fn motion_has_the_declared_horizontal_and_vertical_sample_support() {
    let mut pixels = vec![[0, 0, 0, 255]; 9];
    *pixels.get_mut(4).unwrap() = [255, 255, 255, 255];
    let source = raster(3, 3, &pixels);
    let horizontal = image(req(Blur::Motion { length: 2., angle: 0., samples: 3 }), &source);
    let vertical = image(req(Blur::Motion { length: 2., angle: 90., samples: 3 }), &source);
    close(rgba_at(&horizontal, 0, 1), &[156, 156, 156, 255], 1);
    assert_eq!(rgba_at(&horizontal, 1, 0), &[0, 0, 0, 255]);
    close(rgba_at(&vertical, 1, 0), &[156, 156, 156, 255], 1);
    assert_eq!(rgba_at(&vertical, 0, 1), &[0, 0, 0, 255]);
}
#[test]
fn radial_spin_zoom_and_custom_center_have_distinct_analytic_support() {
    let mut pixels = vec![[0, 0, 0, 255]; 9];
    *pixels.get_mut(1).unwrap() = [255, 255, 255, 255];
    let source = raster(3, 3, &pixels);
    let spin = image(req(Blur::Radial { mode: RadialMode::Spin, center: Point::default(), amount: 1., samples: 3 }), &source);
    let zoom = image(req(Blur::Radial { mode: RadialMode::Zoom, center: Point::default(), amount: 1., samples: 3 }), &source);
    let custom = image(req(Blur::Radial { mode: RadialMode::Spin, center: Point { x: 1. / 6., y: 0.5 }, amount: 1., samples: 3 }), &source);
    // At (2,1), centered spin visits (1,0),(2,1),(1,2); zoom visits the black center-to-right line.
    close(rgba_at(&spin, 2, 1), &[156, 156, 156, 255], 1);
    assert_eq!(rgba_at(&zoom, 2, 1), &[0, 0, 0, 255]);
    // The custom center is pixel (0,1), so the rotated endpoints lie on the black left column.
    assert_eq!(rgba_at(&custom, 2, 1), &[0, 0, 0, 255]);
}
#[test]
fn bokeh_disk_and_hexagon_sample_positions_match_authored_fixture() {
    let mut pixels = vec![[0, 0, 0, 255]; 25];
    for y in 0..5 {
        *pixels.get_mut(y * 5 + 1).unwrap() = [255, 255, 255, 255]
    }
    let source = raster(5, 5, &pixels);
    let disk = image(req(Blur::Bokeh { radius: 3., samples: 2, aperture: Aperture::Disk }), &source);
    let hex = image(req(Blur::Bokeh { radius: 3., samples: 2, aperture: Aperture::Hexagon }), &source);
    // Center sample is black. The second sample x is 0.4358043982 (disk) or 0.6125187072
    // (hexagon); a white column at x=1 contributes that linear amount, then average two samples.
    close(rgba_at(&disk, 2, 2), &[129, 129, 129, 255], 1);
    close(rgba_at(&hex, 2, 2), &[150, 150, 150, 255], 1);
}
#[test]
fn target_selection_and_tilt_shift_outside_band_are_not_identity_or_global_blur() {
    let source = raster(3, 1, &[[255, 0, 0, 255], [0, 0, 0, 255], [0, 0, 0, 255]]);
    let matcher = Matcher { target: Color { r: 255, g: 0, b: 0, a: 255 }, tolerance: 0., softness: 0. };
    let blurred = image(req(Blur::Gaussian { sigma: 1. }), &source);
    let matched = image(req(Blur::TargetColor { sigma: 1., matcher, selection: Selection::Matched }), &source);
    let unmatched = image(req(Blur::TargetColor { sigma: 1., matcher, selection: Selection::Unmatched }), &source);
    assert_eq!(rgba_at(&matched, 0, 0), rgba_at(&blurred, 0, 0));
    assert_eq!(rgba_at(&matched, 1, 0), rgba_at(&source, 1, 0));
    assert_eq!(rgba_at(&unmatched, 0, 0), rgba_at(&source, 0, 0));
    assert_eq!(rgba_at(&unmatched, 1, 0), rgba_at(&blurred, 1, 0));
    let mut pixels = vec![[0, 0, 0, 255]; 15];
    *pixels.get_mut(2).unwrap() = [255, 255, 255, 255];
    let source = raster(5, 3, &pixels);
    let tilt = image(req(Blur::TiltShift { sigma: 1., focus: Point::default(), angle: 0., band_width: 0., falloff: 2. / 3. }), &source);
    // Top-row perpendicular distance is 1/3 short-side units, hence exactly half blur.
    // Independent 2D Gaussian center weight with edge clamping gives encoded result 209.
    close(rgba_at(&tilt, 2, 0), &[209, 209, 209, 255], 1);
    assert_eq!(rgba_at(&tilt, 2, 1), rgba_at(&source, 2, 1));
}

#[test]
fn every_tagged_empty_selector_rejects_unknown_fields_without_wire_changes() {
    fn strict<T: serde::de::DeserializeOwned + serde::Serialize>(names: &[&str]) {
        for name in names {
            let value = serde_json::json!({"kind":name});
            let parsed: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
            for hidden in [serde_json::json!(1), serde_json::json!(null)] {
                let mut invalid = value.clone();
                invalid["hidden"] = hidden;
                assert!(serde_json::from_value::<T>(invalid).is_err(), "{name}");
            }
        }
    }
    strict::<Preset>(&["none", "grayscale", "sepia", "noir"]);
    strict::<Ratio>(&["source", "square", "landscape4_3", "portrait3_4", "landscape16_9", "portrait9_16"]);
    strict::<AspectMode>(&["crop"]);
    strict::<Crop>(&["full"]);
    strict::<Anchor>(&["center", "top_left", "top", "top_right", "left", "right", "bottom_left", "bottom", "bottom_right"]);
    strict::<OverlaySize>(&["source"]);
}
#[test]
fn convolution_and_compound_peak_memory_have_exact_small_boundaries() {
    let source = flat(3, 2, [31, 87, 19, 127]);
    // Gaussian: source24 + output24 + ring(2*3*16) + two4096 fixed allowances =8336.
    // Box: source24 + output24 + ring96 + columns(3*32) +4096 allowance =4336.
    for (request, peak) in [
        (req(Blur::Gaussian { sigma: 1. }), 8336),
        (req(Blur::Box { radius: 1 }), 4336),
        (ImageRequest::Aspect { options: Aspect { mode: AspectMode::Pad { padding: Padding::Blurred { sigma: 1. } }, ..Aspect::default() } }, 8360),
    ] {
        let before = super::raster::allocations();
        assert_eq!(
            request.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), ImageLimits { working_bytes: peak - 1, ..l() }).unwrap_err(),
            ImageError::ScratchLimit
        );
        assert_eq!(super::raster::allocations(), before);
        assert!(request.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), ImageLimits { working_bytes: peak, ..l() }).is_ok());
    }
}
#[test]
fn conservative_decode_orientation_and_rgba_overlap_is_reserved() {
    let meta =
        nodecraft_media::ImageMeta { mime: "image/png", width: 2, height: 3, color: nodecraft_media::ColorModel::Gray, bits_per_channel: 16, has_alpha: false };
    // Other retained8 + decoded12 + orientation12 + RGBA24 +4096 fixed allowance.
    assert!(matches!(super::decode::reserve(meta, 8, ImageLimits { working_bytes: 4151, ..l() }), Err(ImageError::ScratchLimit)));
    assert_eq!(super::decode::reserve(meta, 8, ImageLimits { working_bytes: 4152, ..l() }).unwrap(), 24);
    // One-bit stored gray still expands to eight-bit decoded storage, not fractional bytes.
    let packed = nodecraft_media::ImageMeta { bits_per_channel: 1, ..meta };
    assert!(super::decode::reserve(packed, 8, ImageLimits { working_bytes: 4139, ..l() }).is_err());
    assert_eq!(super::decode::reserve(packed, 8, ImageLimits { working_bytes: 4140, ..l() }).unwrap(), 12);
}
