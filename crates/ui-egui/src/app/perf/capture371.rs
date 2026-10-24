//! Whole CPU show/collection/tessellation path, not a Recorder::record microbenchmark.
use super::*;
use crate::app::metrics::visibility;
use std::{cell::Cell, rc::Rc};

#[test]
#[ignore = "bounded full-path paired capture overhead, run once on demand"]
fn perf371_frame_collection_overhead() {
    for count in [200, 1000] {
        for ppp in [1.0, 2.0] {
            let mut ordered = Vec::new();
            for enabled in [false, true, true, false] {
                let mut h = graph(count, ppp);
                settle(&mut h, 0.3);
                let calls = Rc::new(Cell::new(0usize));
                let c = calls.clone();
                let epoch = Instant::now();
                h.state_mut().session.perf_mut().set_clock(Some(Box::new(move || {
                    c.set(c.get() + 1);
                    u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
                })));
                h.state_mut().session.perf_mut().set_enabled(enabled);
                for _ in 0..30 {
                    h.step();
                }
                h.state_mut().session.perf_mut().clear();
                calls.set(0);
                visibility::take_calls();
                let mut samples = Vec::with_capacity(120);
                let mut callback_count = 0usize;
                let mut curve_count = 0usize;
                for frame in 0..120 {
                    h.state_mut().view = CanvasView { pan: Vec2::new(40.0 + (frame % 40) as f32 * 3.0, 40.0), zoom: 0.2 + (frame % 30) as f32 * 0.005 };
                    h.hover_at(Pos2::new(600.0 + (frame % 40) as f32, 400.0));
                    let start = Instant::now();
                    h.step();
                    let meshes = h.ctx.tessellate(h.output().shapes.clone(), h.output().pixels_per_point);
                    std::hint::black_box(meshes);
                    samples.push(start.elapsed());
                    callback_count += calls.replace(0);
                    curve_count += visibility::take_calls();
                }
                assert_eq!(callback_count > 0, enabled);
                assert_eq!(curve_count > 0, enabled);
                let total = samples.iter().sum::<Duration>();
                let mean = total.as_nanos() / 120;
                // Sorted copy is only for a percentile; keep block totals and raw order intact.
                let mut sorted = samples.clone();
                sorted.sort();
                let p95 = sorted[113].as_nanos();
                let edges = h.state().session.document().canvases()[&h.state().canvas].edges().len();
                let raw: Vec<_> = samples.iter().map(Duration::as_nanos).collect();
                eprintln!(
                    "perf371_frame nodes={count} edges={edges} ppp={ppp} enabled={enabled} frames=120 callbacks={callback_count} diagnostic_curves={curve_count} total_ns={} mean_ns={mean} p95_ns={p95} renderer=egui_kittest_CPU_tessellation os={} arch={} debug={} samples_ns={raw:?}",
                    total.as_nanos(),
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    cfg!(debug_assertions)
                );
                ordered.push((enabled, total.as_secs_f64()));
            }
            let off = ordered.iter().filter(|x| !x.0).map(|x| x.1).sum::<f64>();
            let on = ordered.iter().filter(|x| x.0).map(|x| x.1).sum::<f64>();
            eprintln!(
                "perf371_frame_paired nodes={count} ppp={ppp} ordered={ordered:?} overhead_percent={:?} scope=CPU_show_and_tessellation_no_GPU_or_presentation",
                (off > 0.0).then(|| 100.0 * (on - off) / off)
            );
        }
    }
}
