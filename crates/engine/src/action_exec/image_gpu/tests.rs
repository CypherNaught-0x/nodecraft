use super::*;
use nodecraft_actions::image::params::*;
use nodecraft_actions::image::{CpuExecutor, Raster};

fn raster(width: u32, height: u32, seed: u32) -> Raster {
    let pixels: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            let v = i.wrapping_mul(2_654_435_761).wrapping_add(seed);
            [(v & 0xff) as u8, ((v >> 8) & 0xff) as u8, ((v >> 16) & 0xff) as u8, 128 + ((v >> 24) & 0x7f) as u8]
        })
        .collect();
    Raster::new(width, height, pixels, ImageLimits::default()).unwrap()
}

/// One request of each built-in action family the desktop app routes to the GPU.
fn families() -> Vec<ImageRequest> {
    vec![
        ImageRequest::Blur { options: Blur::Gaussian { sigma: 1.7 } },
        ImageRequest::Grade { options: Grade { warmth: 0.4, brightness: -0.2, contrast: 1.3, saturation: 0.6 } },
        ImageRequest::Filter { options: Filter { preset: Preset::Sepia {}, intensity: 0.7 } },
        ImageRequest::Tint { options: Tint { color: Color { r: 210, g: 20, b: 120, a: 200 }, intensity: 0.6, blend: Blend::Multiply } },
        ImageRequest::Flip { axis: Axis::Both },
        ImageRequest::Rotate { options: Rotate { angle: 90., expand: true, background: Color::TRANSPARENT, resampling: Resampling::Nearest } },
        ImageRequest::Crop { options: Crop::Pixels { x: 1, y: 2, width: 9, height: 7 } },
        ImageRequest::Key { options: Key::default() },
        ImageRequest::ReplaceColor { options: ReplaceColor::default() },
        ImageRequest::Aspect { options: Aspect { ratio: Ratio::Square {}, mode: AspectMode::Crop {}, anchor: Point::default() } },
        ImageRequest::Overlay {
            options: Overlay {
                anchor: Anchor::Custom { point: Point { x: 0.3, y: 0.6 } },
                offset_x: 1,
                offset_y: -1,
                size: OverlaySize::Pixels { width: 6 },
                opacity: 0.7,
                resampling: Resampling::Bilinear,
            },
        },
        ImageRequest::Duplicate { count: 2 },
        ImageRequest::Resize {
            options: Resize { size: ResizeSize::Exact { width: 11, height: 5, fit: Fit::Contain, anchor: Point::default() }, resampling: Resampling::Area },
        },
    ]
}

fn images(result: &ImageResult) -> Vec<&Raster> {
    match result {
        ImageResult::Image(one) => vec![one],
        ImageResult::Images(all) => all.iter().collect(),
    }
}

/// Whether `gpu` matches the CPU reference `cpu` within the documented tolerance: at most 2 in
/// any 8-bit channel, or, where either alpha is at most 2, within 2/255 premultiplied.
fn within_tolerance(cpu: &ImageResult, gpu: &ImageResult) -> Result<(), String> {
    let (c, g) = (images(cpu), images(gpu));
    if c.len() != g.len() {
        return Err(format!("{} images against {}", c.len(), g.len()));
    }
    for (c, g) in c.iter().zip(g) {
        if (c.width(), c.height()) != (g.width(), g.height()) {
            return Err("different sizes".into());
        }
        for (a, b) in c.pixels().as_chunks::<4>().0.iter().zip(g.pixels().as_chunks::<4>().0) {
            let diff = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
            let faint = a[3].min(b[3]) <= 2 && a[3].abs_diff(b[3]) <= 2;
            if diff > 2 && !faint {
                return Err(format!("{a:?} against {b:?}"));
            }
        }
    }
    Ok(())
}

#[test]
fn without_a_compatible_gpu_the_action_is_left_to_the_cpu() {
    let source = raster(16, 12, 1);
    let inputs = ImageInputs { source: &source, overlay: None };
    let none = GpuOptions::without_backends();
    let request = ImageRequest::Flip { axis: Axis::Horizontal };
    // Whatever the slot held before, no backend means no GPU: the CPU takes it.
    *SLOT.lock().unwrap() = Slot::Untried;
    assert!(execute_with(&request, inputs, ImageLimits::default(), &|| false, none).is_none());
    assert!(matches!(&*SLOT.lock().unwrap(), Slot::Unavailable(_)), "remembered, so the next action doesn't look again at once");
    assert!(execute_with(&request, inputs, ImageLimits::default(), &|| false, GpuOptions::default()).is_none(), "not looked for again within a minute");
    *SLOT.lock().unwrap() = Slot::Untried;
}

#[test]
fn the_gpu_path_is_off_unless_the_app_turns_it_on() {
    // No test turns it on: the CPU reference stays exact for every other test in this process.
    assert!(!enabled());
    let source = raster(4, 4, 2);
    assert!(execute(&ImageRequest::Flip { axis: Axis::Vertical }, ImageInputs { source: &source, overlay: None }, ImageLimits::default(), &|| false).is_none());
}

/// On a real GPU (or a software Vulkan driver), every family the app routes there matches the
/// CPU reference within the tolerance, and a cancelled action is handed back to the CPU without
/// keeping the later ones off the GPU.
#[test]
#[ignore = "requires a compatible GPU or software Vulkan driver; run explicitly and record the adapter"]
fn on_a_gpu_every_family_matches_the_cpu_reference() {
    let source = raster(17, 13, 3);
    let overlay = raster(5, 7, 4);
    // A cancelled action goes to the CPU, and doesn't keep the next ones off the GPU.
    let plain = ImageInputs { source: &source, overlay: None };
    let blur = ImageRequest::Blur { options: Blur::Gaussian { sigma: 3. } };
    assert!(execute_with(&blur, plain, ImageLimits::default(), &|| true, GpuOptions::default()).is_none());
    let mut ran = 0;
    for request in families() {
        let inputs = ImageInputs { source: &source, overlay: matches!(request, ImageRequest::Overlay { .. }).then_some(&overlay) };
        let cpu = request.execute(inputs, &mut CpuExecutor::serial(), ImageLimits::default()).unwrap();
        let gpu = execute_with(&request, inputs, ImageLimits::default(), &|| false, GpuOptions::default()).expect("a GPU result");
        within_tolerance(&cpu, &gpu).unwrap_or_else(|why| panic!("{request:?}: {why}"));
        ran += 1;
    }
    assert_eq!(ran, families().len());
    println!("adapter: {:?}", adapter());
}
