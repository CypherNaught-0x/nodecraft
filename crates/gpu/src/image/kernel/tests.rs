use super::*;
use nodecraft_actions::image::Raster;
fn make_source(w: u32, h: u32) -> Raster {
    Raster::new(w, h, vec![255; (w * h * 4) as usize], ImageLimits::default()).unwrap()
}
#[test]
fn byte_work_and_device_limits_select_jointly() {
    let source = make_source(17, 263);
    let request = ImageRequest::Blur { options: Blur::Gaussian { sigma: 2. } };
    let limits = ImageLimits::default();
    let p = request.prepare(ImageInputs::single(&source), limits).unwrap();
    let caps = wgpu::Limits::default();
    let r = select(&p, &caps, 128).unwrap();
    assert_eq!(r.stripes.unwrap().block, 128);
    let peak = p.reference_working_bytes().unwrap() + r.extra_bytes;
    let lower = ImageLimits { working_bytes: peak - 1, ..limits };
    let p_lower = request.prepare(ImageInputs::single(&source), lower).unwrap();
    assert_eq!(select(&p_lower, &caps, 128).unwrap().stripes.unwrap().block, 127);
    let work = p.reference_work().unwrap() + r.extra_work;
    let exact = ImageLimits { work, ..limits };
    let p_exact = request.prepare(ImageInputs::single(&source), exact).unwrap();
    assert_eq!(select(&p_exact, &caps, 128).unwrap().stripes.unwrap().block, 128);
    assert!(select(&p_exact, &caps, 64).is_err());
    let short = ImageLimits { work: work - 1, ..limits };
    let p_short = request.prepare(ImageInputs::single(&source), short).unwrap();
    assert!(select(&p_short, &caps, 128).is_err());
    let mut low = caps.clone();
    low.max_storage_buffer_binding_size = 32768;
    let chosen = select(&p, &low, 128).unwrap();
    assert!(chosen.stripes.unwrap().block < 128);
    assert!(chosen.float_bytes <= 32768);
    let mut low = caps;
    low.max_uniform_buffer_binding_size = 31;
    assert!(select(&p, &low, 128).is_err());
}
#[test]
fn unavoidable_halo_and_stage_inventory_are_admitted_before_pack() {
    let limits = ImageLimits::default();
    let caps = wgpu::Limits::default();
    let source = make_source(17, 263);
    let request = ImageRequest::Blur { options: Blur::Gaussian { sigma: 64. } };
    let p = request.prepare(ImageInputs::single(&source), limits).unwrap();
    let minimum = StripePlan::new(263, 1, 192).unwrap().float_bytes(17).unwrap();
    let small = ImageLimits { working_bytes: p.reference_working_bytes().unwrap() + minimum - 1, ..limits };
    let p = request.prepare(ImageInputs::single(&source), small).unwrap();
    assert!(select(&p, &caps, 128).is_err());
    let source = make_source(1, 16384);
    let request = ImageRequest::Aspect {
        options: Aspect {
            ratio: Ratio::Custom { numerator: 1, denominator: 16384 },
            mode: AspectMode::Pad { padding: Padding::Blurred { sigma: 1. / 3. } },
            anchor: Point::default(),
        },
    };
    let p = request.prepare(ImageInputs::single(&source), limits).unwrap();
    let r = select(&p, &caps, 1).unwrap();
    assert_eq!(r.stages, 32770);
    assert_eq!(r.float_bytes, 48);
    assert!(r.extra_bytes > 18_000_000);
    let small = ImageLimits { working_bytes: 8 * 1024 * 1024, ..limits };
    let p = request.prepare(ImageInputs::single(&source), small).unwrap();
    assert!(select(&p, &caps, 1).is_err());
}
