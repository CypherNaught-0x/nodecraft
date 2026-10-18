use super::*;
use nodecraft_actions::image::{CpuExecutor, Raster};
use std::{
    future::Future,
    task::{Context, Poll},
    time::{Duration, Instant},
};
fn drive<T>(handle: GpuPollHandle, future: impl Future<Output = Result<T, GpuImageError>>) -> Result<T, GpuImageError> {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    let start = Instant::now();
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
        handle.poll_nonblocking()?;
        if start.elapsed() > Duration::from_secs(10) {
            return Err(GpuImageError::Device);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
#[ignore = "requires an actual compatible GPU; run explicitly and record adapter evidence"]
fn actual_gpu_exact_key_and_continuous_color() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    println!("real_adapter name={} backend={:?}", gpu.adapter_info().name, gpu.adapter_info().backend);
    let l = ImageLimits::default();
    let d = (2_f64 / 3.).sqrt();
    for tolerance in [f64::from_bits(d.to_bits() - 1), d, f64::from_bits(d.to_bits() + 1)] {
        let source = Raster::new(1, 1, vec![255, 0, 0, 255], l)?;
        let req = ImageRequest::Key { options: Key { matcher: Matcher { target: Color::GREEN, tolerance, softness: 0. }, mode: KeyMode::RemoveMatched } };
        let ImageResult::Image(cpu) = req.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), l)? else { panic!("image") };
        let result = drive(gpu.poll_handle(), gpu.execute(&req, ImageInputs::single(&source), l))?;
        let ImageResult::Image(actual) = result else { panic!("image") };
        assert_eq!(actual.pixels(), cpu.pixels());
    }
    let t = 2. * d - 1.;
    for tolerance in [f64::from_bits(t.to_bits() - 1), t, f64::from_bits(t.to_bits() + 1)] {
        let source = Raster::new(1, 1, vec![255, 0, 0, 1], l)?;
        let req = ImageRequest::Key { options: Key { matcher: Matcher { target: Color::GREEN, tolerance, softness: 1. }, mode: KeyMode::KeepMatched } };
        let ImageResult::Image(cpu) = req.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), l)? else { panic!("image") };
        let ImageResult::Image(actual) = drive(gpu.poll_handle(), gpu.execute(&req, ImageInputs::single(&source), l))? else { panic!("image") };
        assert_eq!(actual.pixels(), cpu.pixels());
    }
    assert_eq!(gpu.counters().dispatches, 6);
    assert_eq!(gpu.counters().readbacks, 6);
    assert_eq!(gpu.counters().decision_visits, 6);
    println!("actual_gpu_exact_key counters={:?}", gpu.counters());
    Ok(())
}

fn requests() -> Vec<ImageRequest> {
    let mut r = vec![
        ImageRequest::Grade { options: Grade { warmth: 0.4, brightness: -0.7, contrast: 1.3, saturation: 0.6 } },
        ImageRequest::Key { options: Key::default() },
        ImageRequest::ReplaceColor {
            options: ReplaceColor { replace_alpha: true, replacement: Color { r: 220, g: 12, b: 78, a: 31 }, ..ReplaceColor::default() },
        },
        ImageRequest::Duplicate { count: 3 },
    ];
    for preset in [
        Preset::None {},
        Preset::Grayscale {},
        Preset::Sepia {},
        Preset::Noir {},
        Preset::Duotone { shadow: Color { r: 20, g: 40, b: 10, a: 255 }, highlight: Color::WHITE },
    ] {
        r.push(ImageRequest::Filter { options: Filter { preset, intensity: 0.7 } });
    }
    for blend in [Blend::Normal, Blend::Multiply, Blend::Screen, Blend::Overlay, Blend::Darken, Blend::Lighten, Blend::Difference] {
        r.push(ImageRequest::Tint { options: Tint { color: Color { r: 210, g: 20, b: 120, a: 99 }, intensity: 0.6, blend } });
    }
    for axis in [Axis::Horizontal, Axis::Vertical, Axis::Both] {
        r.push(ImageRequest::Flip { axis });
    }
    r.push(ImageRequest::Crop { options: Crop::Pixels { x: 1, y: 1, width: 3, height: 2 } });
    r.push(ImageRequest::Crop { options: Crop::Normalized { x: 0.1, y: 0.2, width: 0.6, height: 0.5 } });
    for resampling in [Resampling::Nearest, Resampling::Bilinear, Resampling::Area] {
        for fit in [Fit::Stretch, Fit::Contain, Fit::Cover] {
            r.push(ImageRequest::Resize {
                options: Resize { size: ResizeSize::Exact { width: 4, height: 9, fit, anchor: Point { x: 0.2, y: 0.8 } }, resampling },
            });
        }
        r.push(ImageRequest::Overlay {
            options: Overlay {
                anchor: Anchor::Custom { point: Point { x: 0.2, y: 0.7 } },
                offset_x: -1,
                offset_y: 1,
                size: OverlaySize::Pixels { width: 4 },
                opacity: 0.6,
                resampling,
            },
        });
    }
    for resampling in [Resampling::Nearest, Resampling::Bilinear] {
        for angle in [0., 90., -90., 180., 33.5] {
            for background in [Color::TRANSPARENT, Color { r: 110, g: 20, b: 90, a: 127 }] {
                r.push(ImageRequest::Rotate { options: Rotate { angle, expand: true, background, resampling } });
            }
        }
    }
    for mode in [
        AspectMode::Crop {},
        AspectMode::Pad { padding: Padding::Solid { color: Color { r: 0, g: 140, b: 220, a: 99 } } },
        AspectMode::Pad { padding: Padding::Blurred { sigma: 1.2 } },
    ] {
        r.push(ImageRequest::Aspect { options: Aspect { ratio: Ratio::Square {}, mode, anchor: Point { x: 0.2, y: 0.8 } } });
    }
    let blurs = vec![
        Blur::Gaussian { sigma: 1.7 },
        Blur::Gaussian { sigma: 1. / 256. },
        Blur::Box { radius: 3 },
        Blur::Motion { length: 8., angle: 33., samples: 17 },
        Blur::Motion { length: 8., angle: 33., samples: 1 },
        Blur::Radial { mode: RadialMode::Spin, center: Point { x: 0.2, y: 0.7 }, amount: 0.6, samples: 17 },
        Blur::Radial { mode: RadialMode::Zoom, center: Point::default(), amount: 0.8, samples: 1 },
        Blur::Bilateral { radius: 3, spatial_sigma: 1.5, range_sigma: 0.2 },
        Blur::Bilateral { radius: 2, spatial_sigma: 1. / 256., range_sigma: 1. / 65536. },
        Blur::Bokeh { radius: 4., samples: 25, aperture: Aperture::Disk },
        Blur::Bokeh { radius: 3., samples: 25, aperture: Aperture::Hexagon },
        Blur::TiltShift { sigma: 1.4, focus: Point { x: 0.1, y: 0.9 }, angle: 27., band_width: 0.2, falloff: 0.3 },
        Blur::TargetColor { sigma: 1.3, matcher: Matcher::default(), selection: Selection::Matched },
        Blur::TargetColor { sigma: 1.3, matcher: Matcher::default(), selection: Selection::Unmatched },
        Blur::Gaussian { sigma: 0. },
        Blur::Box { radius: 0 },
    ];
    for options in blurs {
        r.push(ImageRequest::Blur { options });
    }
    r
}
fn linear_byte(v: u8) -> f64 {
    let v = f64::from(v) / 255.;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}
fn compare(label: &str, cpu: &ImageResult, gpu: &ImageResult, adversarial: bool, raw: bool) {
    let c: Vec<_> = match cpu {
        ImageResult::Image(r) => vec![r],
        ImageResult::Images(r) => r.iter().collect(),
    };
    let g: Vec<_> = match gpu {
        ImageResult::Image(r) => vec![r],
        ImageResult::Images(r) => r.iter().collect(),
    };
    assert_eq!(c.len(), g.len());
    let mut max = 0;
    let mut exceptions = 0;
    for (c, g) in c.iter().zip(g) {
        assert_eq!((c.width(), c.height()), (g.width(), g.height()), "{label}");
        for (a, b) in c.pixels().as_chunks::<4>().0.iter().zip(g.pixels().as_chunks::<4>().0.iter()) {
            if raw {
                assert_eq!(a, b, "exact raw {label}");
            }
            if !raw && b[3] == 0 {
                assert_eq!(&b[..3], &[0; 3], "computed zero canonical {label}");
            }
            let diff = a.iter().zip(b).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            max = max.max(diff);
            if diff > 2 && adversarial && a[3].min(b[3]) <= 2 {
                assert!(a[3].abs_diff(b[3]) <= 2, "alpha {label}: {a:?}/{b:?}");
                for (x, y) in a.iter().zip(b).take(3) {
                    let delta = (linear_byte(*x) * f64::from(a[3]) / 255. - linear_byte(*y) * f64::from(b[3]) / 255.).abs();
                    assert!(delta <= 2. / 255., "premul {label}: {a:?}/{b:?}");
                }
                exceptions += 1;
            } else {
                assert!(diff <= 2, "straight {label}: {a:?}/{b:?} error{diff}");
            }
            if b[3] == 0 && a[3] != 0 {
                assert_eq!(&b[..3], &[0; 3], "computed zero canonical {label}");
            }
        }
    }
    println!("matrix {label} straight_max={max} low_alpha_exceptions={exceptions}");
}
#[test]
#[ignore = "requires an actual compatible GPU; no-device is an error, not a passing GPU result"]
fn actual_gpu_all_request_families() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let requests = requests();
    let mut count = 0;
    for adversarial in [false, true] {
        let mut bytes = Vec::new();
        for y in 0..5u8 {
            for x in 0..7u8 {
                bytes.extend_from_slice(&[x * 37, y * 51, (x + y) * 19, if adversarial { [0, 1, 2, 127, 254, 255][usize::from(x + y) % 6] } else { 255 }]);
            }
        }
        let source = Raster::new(7, 5, bytes, l)?;
        let overlay = Raster::new(3, 2, vec![240, 20, 80, 127, 0, 255, 0, 255, 255, 0, 0, 0, 120, 200, 30, 1, 0, 0, 200, 254, 90, 30, 170, 200], l)?;
        for (i, request) in requests.iter().enumerate() {
            let inputs = ImageInputs { source: &source, overlay: if matches!(request, ImageRequest::Overlay { .. }) { Some(&overlay) } else { None } };
            let cpu = request.execute(inputs, &mut CpuExecutor::serial(), l)?;
            let actual = drive(gpu.poll_handle(), gpu.execute(request, inputs, l))?;
            let raw = request.prepare(inputs, l)?.identity()
                || matches!(
                    request,
                    ImageRequest::Duplicate { .. }
                        | ImageRequest::Flip { .. }
                        | ImageRequest::Crop { .. }
                        | ImageRequest::Resize { options: Resize { resampling: Resampling::Nearest, .. } }
                        | ImageRequest::Aspect { options: Aspect { mode: AspectMode::Crop {}, .. } }
                )
                || matches!(request,ImageRequest::Rotate{options} if options.background.a==0&&options.angle.rem_euclid(90.)==0.&&request.prepare(inputs,l)?.nearest_rotation_indices()?.is_some());
            compare(&format!("case{i} adversarial{adversarial} {request:?}"), &cpu, &actual, adversarial, raw);
            count += 1;
        }
    }
    println!("all_families cases={count} counters={:?}", gpu.counters());
    assert!(gpu.counters().dispatches > count);
    assert!(gpu.counters().readbacks > 0);
    Ok(())
}

#[test]
#[ignore = "requires actual GPU to prove scoped submission errors never reach default panic handler"]
fn actual_gpu_submission_failure_is_contained() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    gpu.invalid_submission = true;
    let l = ImageLimits::default();
    let source = Raster::new(1, 1, vec![255; 4], l)?;
    let request = ImageRequest::Key { options: Key::default() };
    let result = drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l));
    assert_eq!(result.unwrap_err(), GpuImageError::Device);
    assert_eq!(source.pixels(), [255; 4]);
    assert!(gpu.pending.is_some());
    assert!(gpu.failed);
    assert_eq!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err(), GpuImageError::Device);
    Ok(())
}
#[test]
#[ignore = "requires actual GPU for cancelled-future pending ownership"]
fn actual_gpu_drop_retains_one_submission() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let handle = gpu.poll_handle();
    let l = ImageLimits::default();
    let source = Raster::new(7, 5, vec![127; 140], l)?;
    let request = ImageRequest::Key { options: Key::default() };
    {
        let mut future = std::pin::pin!(gpu.execute(&request, ImageInputs::single(&source), l));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(future.as_mut().poll(&mut cx).is_pending());
    }
    assert!(gpu.pending.is_some());
    assert_eq!(gpu.counters().dispatches, 1);
    if !gpu.pending.as_ref().unwrap().complete.load(Ordering::Acquire) {
        assert_eq!(drive(handle.clone(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err(), GpuImageError::Busy);
    }
    let start = Instant::now();
    while !gpu.pending.as_ref().unwrap().complete.load(Ordering::Acquire) {
        handle.poll_nonblocking()?;
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
    let _ = drive(handle, gpu.execute(&request, ImageInputs::single(&source), l))?;
    assert!(gpu.pending.is_none());
    assert_eq!(gpu.counters().dispatches, 2);
    assert_eq!(gpu.counters().readbacks, 1);
    Ok(())
}

#[test]
#[ignore = "actual GPU semantic regression: arbitrary nearest rotation is computed, not a raw permutation"]
fn actual_gpu_arbitrary_nearest_rotation_canonicalizes_hidden_rgb() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let source = Raster::new(3, 3, [222, 93, 7, 0].repeat(9), l)?;
    let request = ImageRequest::Rotate { options: Rotate { angle: 33.5, expand: true, background: Color::TRANSPARENT, resampling: Resampling::Nearest } };
    let ImageResult::Image(cpu) = request.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), l)? else { panic!("image") };
    let ImageResult::Image(actual) = drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l))? else { panic!("image") };
    println!("arbitrary_nearest cpu={:?} gpu={:?}", cpu.pixels(), actual.pixels());
    assert_eq!(actual.pixels(), cpu.pixels());
    assert!(actual.pixels().iter().all(|v| *v == 0));
    Ok(())
}

#[test]
#[ignore = "actual GPU registration failure must be scoped, not a default panic"]
fn actual_gpu_map_registration_failure_is_contained() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    gpu.invalid_map = true;
    let l = ImageLimits::default();
    let source = Raster::new(1, 1, vec![255; 4], l)?;
    let request = ImageRequest::Key { options: Key::default() };
    let error = drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err();
    assert_eq!(error, GpuImageError::Device);
    assert_eq!(source.pixels(), [255; 4]);
    Ok(())
}

#[test]
#[ignore = "actual GPU exact logical admission boundary and real positive work counters"]
fn actual_gpu_limits_before_proportional_preparation() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let base = ImageLimits::default();
    let source = Raster::new(1, 1, vec![255; 4], base)?;
    let request = ImageRequest::Key { options: Key::default() };
    // 4104 reference bytes +17020 alternate lifetime bytes =21124; one byte less refuses
    // before matcher allocation/upload/dispatch. This includes three decision representations.
    let denied = ImageLimits { working_bytes: 21123, ..base };
    assert_eq!(
        drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), denied)).unwrap_err(),
        GpuImageError::Action(ImageError::ScratchLimit)
    );
    assert_eq!(gpu.counters().decision_visits, 0);
    assert_eq!(gpu.counters().uploaded_bytes, 0);
    assert_eq!(gpu.counters().dispatches, 0);
    let denied = ImageLimits { work: 1, ..base };
    assert_eq!(
        drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), denied)).unwrap_err(),
        GpuImageError::Action(ImageError::WorkLimit)
    );
    assert_eq!(gpu.counters().decision_visits, 0);
    let accepted = ImageLimits { working_bytes: 21124, work: 1037, ..base };
    let _ = drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), accepted))?;
    assert_eq!(gpu.counters().decision_visits, 1);
    assert_eq!(gpu.counters().dispatches, 1);
    assert_eq!(gpu.counters().readbacks, 1);
    assert!(gpu.counters().uploaded_bytes > 0);
    for denied in
        [ImageLimits { input_bytes: 3, ..base }, ImageLimits { output_bytes: 3, ..base }, ImageLimits { outputs: 0, ..base }, ImageLimits { edge: 0, ..base }]
    {
        assert!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), denied)).is_err());
    }
    assert_eq!(gpu.counters().dispatches, 1);
    assert_eq!(source.pixels(), [255; 4]);
    Ok(())
}
#[test]
#[ignore = "actual GPU single-row/column, maximum supports and identity/raw contracts"]
fn actual_gpu_edge_shapes_and_extremes() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let mut cases = vec![
        ImageRequest::Blur { options: Blur::Gaussian { sigma: 64. } },
        ImageRequest::Blur { options: Blur::Box { radius: 128 } },
        ImageRequest::Blur { options: Blur::Bilateral { radius: 8, spatial_sigma: 8., range_sigma: 1. } },
        ImageRequest::Blur { options: Blur::Motion { length: 256., angle: 90., samples: 129 } },
        ImageRequest::Blur { options: Blur::Radial { mode: RadialMode::Spin, center: Point { x: 0., y: 1. }, amount: 1., samples: 129 } },
        ImageRequest::Blur { options: Blur::Bokeh { radius: 64., samples: 129, aperture: Aperture::Hexagon } },
    ];
    cases.push(ImageRequest::Rotate { options: Rotate { angle: 90., expand: false, background: Color::TRANSPARENT, resampling: Resampling::Bilinear } });
    for (w, h) in [(1, 1), (1, 7), (9, 1), (8, 6)] {
        let mut bytes = Vec::new();
        for i in 0..w * h {
            bytes.extend_from_slice(&[(i * 73 % 256) as u8, (i * 41 % 256) as u8, (i * 109 % 256) as u8, 255]);
        }
        let source = Raster::new(w, h, bytes, l)?;
        for (i, req) in cases.iter().enumerate() {
            let inputs = ImageInputs::single(&source);
            let cpu = req.execute(inputs, &mut CpuExecutor::serial(), l)?;
            let actual = drive(gpu.poll_handle(), gpu.execute(req, inputs, l))?;
            let raw = matches!(req, ImageRequest::Rotate { .. }) && req.prepare(inputs, l)?.nearest_rotation_indices()?.is_some();
            compare(&format!("extreme{i} {w}x{h}"), &cpu, &actual, false, raw);
        }
    }
    assert!(gpu.counters().coefficient_visits > 0);
    println!("extreme counters={:?}", gpu.counters());
    Ok(())
}

#[test]
#[ignore = "actual GPU multiple global halos, adaptive peak/work admission and compound quantization"]
fn actual_gpu_tall_stripes_and_adaptive_budget() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let make = |w: u32, h: u32| {
        let mut bytes = Vec::new();
        for y in 0..h {
            for x in 0..w {
                bytes.extend_from_slice(&[((x * 73 + y * 19) % 256) as u8, ((y * 41 + x * 3) % 256) as u8, ((x * 109 + y * 53) % 256) as u8, 255]);
            }
        }
        Raster::new(w, h, bytes, l)
    };
    let source = make(17, 263)?;
    let cases = [
        ImageRequest::Blur { options: Blur::Gaussian { sigma: 4. } },
        ImageRequest::Blur { options: Blur::Box { radius: 128 } },
        ImageRequest::Blur { options: Blur::TiltShift { sigma: 3., focus: Point { x: 0.2, y: 0.73 }, angle: 27., band_width: 0.25, falloff: 0.2 } },
        ImageRequest::Blur {
            options: Blur::TargetColor { sigma: 3., matcher: Matcher { tolerance: 0.4, softness: 0.2, ..Matcher::default() }, selection: Selection::Unmatched },
        },
        ImageRequest::Aspect {
            options: Aspect {
                ratio: Ratio::Custom { numerator: 1, denominator: 20 },
                mode: AspectMode::Pad { padding: Padding::Blurred { sigma: 2. } },
                anchor: Point { x: 0.3, y: 0.8 },
            },
        },
    ];
    for block in [2, 3, 128] {
        gpu.max_stripe_height = block;
        for (i, req) in cases.iter().enumerate() {
            let inputs = ImageInputs::single(&source);
            let cpu = req.execute(inputs, &mut CpuExecutor::serial(), l)?;
            let actual = drive(gpu.poll_handle(), gpu.execute(req, inputs, l))?;
            compare(&format!("stripeB{block} case{i}"), &cpu, &actual, false, false);
            assert!(gpu.counters().last_stripe_height <= block);
            println!("stripeB{block} case{i} counters={:?}", gpu.counters());
        }
    }
    gpu.max_stripe_height = 1;
    let narrow = make(1, 17)?;
    let req = ImageRequest::Blur { options: Blur::Box { radius: 128 } };
    let inputs = ImageInputs::single(&narrow);
    let cpu = req.execute(inputs, &mut CpuExecutor::serial(), l)?;
    let actual = drive(gpu.poll_handle(), gpu.execute(&req, inputs, l))?;
    compare("stripeB1 radius>height", &cpu, &actual, false, false);
    gpu.max_stripe_height = 128;
    let req = ImageRequest::Blur { options: Blur::Gaussian { sigma: 2. } };
    let p = req.prepare(ImageInputs::single(&source), l)?;
    let r = kernel::select(&p, &gpu.device.limits(), 128)?;
    assert_eq!(r.stripes.unwrap().block, 128);
    let peak = p.reference_working_bytes()?.checked_add(r.extra_bytes).unwrap();
    let budget = ImageLimits { working_bytes: peak - 1, ..l };
    let actual = drive(gpu.poll_handle(), gpu.execute(&req, ImageInputs::single(&source), budget))?;
    let cpu = req.execute(ImageInputs::single(&source), &mut CpuExecutor::serial(), budget)?;
    compare("adaptive128fails127fits", &cpu, &actual, false, false);
    assert_eq!(gpu.counters().last_stripe_height, 127);
    assert!(gpu.counters().planning_candidates > 1);
    Ok(())
}

#[test]
#[ignore = "actual GPU admitted 16MiB area-reduction precision regression"]
fn actual_gpu_large_area_constant_precision() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let source = Raster::new(2048, 2048, [128, 128, 128, 255].repeat(2048 * 2048), l)?;
    let request = ImageRequest::Resize {
        options: Resize { size: ResizeSize::Exact { width: 1, height: 1, fit: Fit::Stretch, anchor: Point::default() }, resampling: Resampling::Area },
    };
    let inputs = ImageInputs::single(&source);
    let cpu = request.execute(inputs, &mut CpuExecutor::serial(), l)?;
    let actual = drive(gpu.poll_handle(), gpu.execute(&request, inputs, l))?;
    compare("2048square_constant_area", &cpu, &actual, false, false);
    Ok(())
}

#[test]
#[ignore = "actual GPU failed initial upload and cancelled map are bounded errors"]
fn actual_gpu_upload_and_map_callback_failures() -> Result<(), Box<dyn std::error::Error>> {
    let l = ImageLimits::default();
    let source = Raster::new(1, 1, vec![255; 4], l)?;
    let request = ImageRequest::Key { options: Key::default() };
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    gpu.invalid_upload = true;
    assert_eq!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err(), GpuImageError::Device);
    assert_eq!(gpu.counters().dispatches, 0);
    assert!(gpu.pending.is_none());
    assert!(gpu.failed);
    let before = gpu.counters();
    assert_eq!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err(), GpuImageError::Device);
    assert_eq!(gpu.counters().decision_visits, before.decision_visits);
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    gpu.cancel_map = true;
    assert_eq!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err(), GpuImageError::Mapping);
    assert_eq!(gpu.counters().readbacks, 0);
    assert!(gpu.pending.is_some());
    assert!(!gpu.pending.as_ref().unwrap().mapped.load(Ordering::Acquire));
    Ok(())
}
#[test]
#[ignore = "actual GPU destroyed-device error path, no fake success"]
fn actual_gpu_destroyed_device_refuses_output() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    gpu.device().destroy();
    let l = ImageLimits::default();
    let source = Raster::new(1, 1, vec![255; 4], l)?;
    let request = ImageRequest::Key { options: Key::default() };
    assert!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).is_err());
    assert_eq!(gpu.counters().readbacks, 0);
    assert!(gpu.failed || gpu.lost.load(Ordering::Acquire));
    let before = gpu.counters();
    assert_eq!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), l)).unwrap_err(), GpuImageError::Device);
    assert_eq!(gpu.counters().decision_visits, before.decision_visits);
    assert_eq!(source.pixels(), [255; 4]);
    Ok(())
}
#[test]
#[ignore = "actual GPU useful medium execution and large-shape preflight refusal"]
fn actual_gpu_medium_and_large_small_budget() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let request = ImageRequest::Blur { options: Blur::Gaussian { sigma: 2. } };
    let mut bytes = Vec::new();
    for y in 0..193u32 {
        for x in 0..256u32 {
            bytes.extend_from_slice(&[((x + y) % 256) as u8, ((x * 7 + y * 3) % 256) as u8, ((x * 3 + y * 11) % 256) as u8, 255]);
        }
    }
    let source = Raster::new(256, 193, bytes, l)?;
    let inputs = ImageInputs::single(&source);
    let cpu = request.execute(inputs, &mut CpuExecutor::serial(), l)?;
    let actual = drive(gpu.poll_handle(), gpu.execute(&request, inputs, l))?;
    compare("medium256x193", &cpu, &actual, false, false);
    assert_eq!(gpu.counters().last_stripe_height, 128);
    assert!(gpu.counters().last_float_bytes < 256 * 193 * 16);
    println!("medium counters={:?}", gpu.counters());
    let source = Raster::new(512, 512, vec![255; 512 * 512 * 4], l)?;
    let small = ImageLimits { working_bytes: 2_500_000, ..l };
    assert!(request.prepare(ImageInputs::single(&source), small).is_ok());
    let before = gpu.counters();
    assert_eq!(drive(gpu.poll_handle(), gpu.execute(&request, ImageInputs::single(&source), small)).unwrap_err(), GpuImageError::Resource);
    assert_eq!(gpu.counters().dispatches, before.dispatches);
    assert_eq!(gpu.counters().uploaded_bytes, before.uploaded_bytes);
    assert_eq!(gpu.counters().decision_visits, before.decision_visits);
    Ok(())
}

#[test]
#[ignore = "actual GPU area mixed axes, fractional footprints and premultiplied roles"]
fn actual_gpu_area_fractional_mixed_axes() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let mut pixels = Vec::new();
    for i in 0..35u32 {
        pixels.extend_from_slice(&[((i * 37) % 256) as u8, ((i * 73) % 256) as u8, ((i * 19) % 256) as u8, if i % 2 == 0 { 255 } else { 127 }]);
    }
    let source = Raster::new(7, 5, pixels, l)?;
    let overlay = Raster::new(3, 2, vec![255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 255, 127, 255, 255, 255, 255, 0, 0, 0, 127, 255, 0, 255, 255], l)?;
    let requests = [
        ImageRequest::Resize {
            options: Resize { size: ResizeSize::Exact { width: 4, height: 9, fit: Fit::Stretch, anchor: Point::default() }, resampling: Resampling::Area },
        },
        ImageRequest::Resize {
            options: Resize { size: ResizeSize::Exact { width: 11, height: 2, fit: Fit::Stretch, anchor: Point::default() }, resampling: Resampling::Area },
        },
        ImageRequest::Resize {
            options: Resize {
                size: ResizeSize::Exact { width: 3, height: 3, fit: Fit::Cover, anchor: Point { x: 0.2, y: 0.8 } },
                resampling: Resampling::Area,
            },
        },
        ImageRequest::Overlay {
            options: Overlay {
                anchor: Anchor::Center {},
                offset_x: -1,
                offset_y: 1,
                size: OverlaySize::Pixels { width: 2 },
                opacity: 0.7,
                resampling: Resampling::Area,
            },
        },
    ];
    for (i, request) in requests.iter().enumerate() {
        let inputs = ImageInputs { source: &source, overlay: if matches!(request, ImageRequest::Overlay { .. }) { Some(&overlay) } else { None } };
        let cpu = request.execute(inputs, &mut CpuExecutor::serial(), l)?;
        let actual = drive(gpu.poll_handle(), gpu.execute(request, inputs, l))?;
        compare(&format!("area_fractional{i}"), &cpu, &actual, false, false);
    }
    Ok(())
}

#[test]
#[ignore = "actual GPU established CPU golden fixtures retain their original expected bytes"]
fn actual_gpu_reference_golden_fixtures() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let fixtures: serde_json::Value = serde_json::from_str(include_str!("../../../actions/tests/fixtures/image_requests.json"))?;
    let mut count = 0;
    for fixture in fixtures.as_array().unwrap() {
        let request = ImageRequest::from_json_bytes(&serde_json::to_vec(fixture.get("request").unwrap())?)?;
        let bytes: Vec<_> = fixture.get("source").unwrap().as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        let expected: Vec<_> = fixture.get("expected").unwrap().as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        let source = Raster::new(1, 1, bytes, l)?;
        let inputs = ImageInputs::single(&source);
        let cpu = request.execute(inputs, &mut CpuExecutor::serial(), l)?;
        let ImageResult::Image(c) = &cpu else { panic!("image") };
        assert_eq!(c.pixels(), expected);
        let actual = drive(gpu.poll_handle(), gpu.execute(&request, inputs, l))?;
        let raw = request.prepare(inputs, l)?.identity() || matches!(request, ImageRequest::Flip { .. });
        compare(fixture.get("name").unwrap().as_str().unwrap(), &cpu, &actual, false, raw);
        count += 1;
    }
    assert_eq!(count, 18);
    println!("unchanged_reference_golden_cases={count}");
    Ok(())
}
#[test]
#[ignore = "actual GPU exact shared identity/duplicate ownership and active invalid fields"]
fn actual_gpu_identity_sharing_and_active_validation() -> Result<(), Box<dyn std::error::Error>> {
    let mut gpu = futures::executor::block_on(GpuExecutor::request())?;
    let l = ImageLimits::default();
    let source = Raster::new(1, 1, vec![17, 93, 7, 0], l)?;
    let req = ImageRequest::Grade { options: Grade::default() };
    let ImageResult::Image(out) = drive(gpu.poll_handle(), gpu.execute(&req, ImageInputs::single(&source), l))? else { panic!("image") };
    assert!(std::ptr::eq(source.pixels().as_ptr(), out.pixels().as_ptr()));
    let req = ImageRequest::Duplicate { count: 3 };
    let ImageResult::Images(out) = drive(gpu.poll_handle(), gpu.execute(&req, ImageInputs::single(&source), l))? else { panic!("images") };
    assert_eq!(out.len(), 3);
    for out in out {
        assert!(std::ptr::eq(source.pixels().as_ptr(), out.pixels().as_ptr()));
    }
    let req = ImageRequest::Resize {
        options: Resize {
            size: ResizeSize::Exact { width: 1, height: 1, fit: Fit::Stretch, anchor: Point { x: f64::NAN, y: 0.5 } },
            resampling: Resampling::Nearest,
        },
    };
    assert!(drive(gpu.poll_handle(), gpu.execute(&req, ImageInputs::single(&source), l)).is_err());
    assert_eq!(gpu.counters().dispatches, 0);
    assert_eq!(gpu.counters().uploaded_bytes, 0);
    Ok(())
}

#[test]
fn disabled_backends_are_an_explicit_error() {
    let options = GpuOptions { backends: wgpu::Backends::empty(), ..GpuOptions::default() };
    assert!(matches!(futures::executor::block_on(GpuExecutor::request_with_options(options)), Err(GpuImageError::Unavailable)));
}
