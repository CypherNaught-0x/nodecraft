use super::*;
#[allow(dead_code)]
mod support {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test-support/perf152.rs"));
}
#[test]
#[ignore = "release CPU regression collection; exact cargo xtask perf filter"]
fn canvas_regression() {
    support::require_release();
    let mut records = vec![];
    for count in support::SIZES {
        for ppp in [1., 2.] {
            for script in ["pan", "zoom"] {
                let mut h = graph(count, ppp);
                let zoom = if script == "pan" && count == 1000 { 1. } else { 0.1 };
                settle(&mut h, zoom);
                h.state_mut().session.perf_mut().set_clock(nodecraft_engine::perf::native_clock());
                h.state_mut().session.perf_mut().set_enabled(true);
                for _ in 0..10 {
                    h.step();
                }
                h.state_mut().session.perf_mut().clear();
                let stats = if script == "pan" {
                    frames(&mut h, 120, |h, frame| {
                        let phase = if (frame / 20) % 2 == 0 { 1. } else { -1. };
                        h.state_mut().view.pan += Vec2::new(6., 3.) * phase;
                    })
                } else {
                    frames(&mut h, 160, |h, frame| {
                        let factor = if frame < 80 { 1.03 } else { 1. / 1.03 };
                        let origin = h.state().viewport.min;
                        h.state_mut().view.zoom_at(origin, Pos2::new(800., 500.), factor);
                    })
                };
                for workload in &stats.workloads {
                    check_workload(count, script, workload);
                }
                let mut record = support::record(
                    format!("canvas/{count}/{ppp}/{script}"),
                    "canvas_cpu",
                    "grid",
                    count,
                    script,
                    10,
                    stats.raw_ns,
                    json!({"span":"egui pass including AccessKit and capture callbacks/scans plus shapes clone and CPU tessellation; untimed workload report observation excluded",
                "viewport":[1600,1000],"ppp":if ppp==1.{1}else{2},"initial_zoom":zoom,"renderer":"egui_kittest CPU tessellation; no window/GPU/presentation","capture":true,
                "visibility":if script=="zoom"{"zoom_sweep"}else if count==200{"all_visible"}else{"mostly_offscreen"},"workloads":stats.workloads}),
                );
                *record.get_mut("perf2").unwrap() = stats.report.unwrap();
                records.push(record);
            }
        }
    }
    support::finish(records);
}

fn check_workload(count: usize, script: &str, workload: &serde_json::Value) {
    assert_eq!(workload.get("total_nodes").and_then(serde_json::Value::as_u64), Some(u64::try_from(count).unwrap()));
    assert!(workload.get("visible_edges").and_then(serde_json::Value::as_u64).is_some());
    let visible = workload.get("visible_nodes").and_then(serde_json::Value::as_u64).unwrap();
    if script == "pan" && count == 200 {
        assert_eq!(visible, 200, "all-visible fixture changed");
    }
    if script == "pan" && count == 1000 {
        assert!(visible < 500, "mostly-offscreen fixture changed");
    }
}
#[test]
fn perf152_canvas_visibility_fixtures_match_their_labels() {
    for count in support::SIZES {
        for ppp in [1., 2.] {
            let mut h = graph(count, ppp);
            settle(&mut h, if count == 200 { 0.1 } else { 1. });
            h.state_mut().session.perf_mut().set_clock(nodecraft_engine::perf::native_clock());
            h.state_mut().session.perf_mut().set_enabled(true);
            h.step();
            check_workload(count, "pan", &serde_json::to_value(h.state().session.perf().report().workload).unwrap());
        }
    }
}
