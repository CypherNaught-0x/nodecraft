use super::super::tests::harness;
use serde_json::{Value, json};

fn metric<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == name).unwrap()
}

#[test]
fn frames_are_timed_only_while_a_capture_runs() {
    let mut h = harness();
    h.run();
    let idle = h.state_mut().session.dispatch("perf.report", &json!({})).unwrap();
    assert_eq!(metric(&idle, "frame_prepare")["samples"], 0, "nothing before perf.start");
    assert_eq!(idle["clock"], true, "the desktop shell supplies a clock");
    h.state_mut().session.dispatch("perf.start", &json!({})).unwrap();
    for _ in 0..5 {
        h.step();
    }
    let report = h.state_mut().session.dispatch("perf.report", &json!({})).unwrap();
    let prepare = metric(&report, "frame_prepare");
    assert!(prepare["samples"].as_u64().unwrap() >= 5, "{prepare}");
    assert!(prepare["p50"].is_u64());
    assert!(metric(&report, "frame_interval")["samples"].as_u64().unwrap() >= 4);
    assert_eq!(report["workload"]["total_nodes"], 1);
    assert_eq!(report["workload"]["visible_nodes"], 1);
    h.state_mut().session.dispatch("perf.stop", &json!({})).unwrap();
    let before = metric(&h.state_mut().session.dispatch("perf.report", &json!({})).unwrap(), "frame_prepare")["samples"].clone();
    h.step();
    let after = metric(&h.state_mut().session.dispatch("perf.report", &json!({})).unwrap(), "frame_prepare")["samples"].clone();
    assert_eq!(before, after, "stopped means stopped");
}

#[test]
fn crossing_curve_with_both_cards_outside_has_visible_workload() {
    use super::super::{CanvasView, tests::chain_harness};
    use egui::Vec2;
    let mut h = chain_harness();
    let canvas = h.state().canvas;
    for (id, x, y) in [(1, -800.0, 200.0), (2, 1500.0, 200.0), (3, 2000.0, 200.0), (4, -1000.0, -1000.0)] {
        h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":nodecraft_types::NodeId::from_bytes([id;16]),"pos":{"x":x,"y":y}})).unwrap();
    }
    h.state_mut().view = CanvasView { pan: Vec2::ZERO, zoom: 1.0 };
    h.state_mut().session.dispatch("perf.start", &json!({})).unwrap();
    h.step();
    assert!(h.state().painted.is_empty(), "neither endpoint card is painted");
    fn crossing(shape: &egui::Shape, clip: egui::Rect) -> bool {
        match shape {
            egui::Shape::CubicBezier(c) => {
                c.points[0].x < clip.left() && c.points[3].x > clip.right() && c.points.iter().all(|p| p.y > clip.top() && p.y < clip.bottom())
            }
            egui::Shape::Vec(shapes) => shapes.iter().any(|s| crossing(s, clip)),
            _ => false,
        }
    }
    assert!(h.output().shapes.iter().any(|s| crossing(&s.shape, s.clip_rect)), "actual submitted cubic crosses its clip");
    let report = h.state_mut().session.dispatch("perf.report", &json!({})).unwrap();
    assert_eq!(report["workload"]["visible_edges"], 1, "a painted crossing curve cannot disappear from workload just because both cards are outside");
    // The draw loop excludes a moving committed edge even if a transient preview is shown.
    h.state_mut().wiring = Some(super::super::Wiring {
        from: nodecraft_graph::Endpoint { node: nodecraft_types::NodeId::from_bytes([1; 16]), port: "out".into() },
        port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: false },
        moving: Some(nodecraft_types::EdgeId::from_bytes([10; 16])),
    });
    h.step();
    assert_eq!(h.state_mut().session.dispatch("perf.report", &json!({})).unwrap()["workload"]["visible_edges"], 0);
    h.state_mut().wiring = None;
    h.state_mut().view.pan.y = 2000.0;
    h.step();
    assert_eq!(h.state_mut().session.dispatch("perf.report", &json!({})).unwrap()["workload"]["visible_edges"], 0, "fully offscreen negative");
}

#[test]
fn disabled_capture_bypasses_geometry_and_invalid_geometry_is_not_zero() {
    use super::visibility;
    let mut h = super::super::tests::chain_harness();
    visibility::take_calls();
    h.step();
    assert_eq!(visibility::take_calls(), 0);
    h.state_mut().session.dispatch("perf.start", &json!({})).unwrap();
    h.step();
    assert!(visibility::take_calls() > 0);
    h.ctx.tessellation_options_mut(|o| o.bezier_tolerance = f32::NAN);
    h.step();
    assert_eq!(visibility::take_calls(), 0, "invalid config refused before callback");
    let report = h.state_mut().session.dispatch("perf.report", &json!({})).unwrap();
    assert!(report["workload"]["visible_edges"].is_null());
    assert_eq!(report["workload"]["visibility_unavailable"], "invalid_curve_geometry_or_options");
    h.ctx.tessellation_options_mut(|o| o.bezier_tolerance = 0.1);
    h.step();
    let report = h.state_mut().session.dispatch("perf.report", &json!({})).unwrap();
    assert!(report["workload"]["visible_edges"].is_u64());
    assert!(report["workload"]["visibility_unavailable"].is_null());
    h.state_mut().session.dispatch("perf.stop", &json!({})).unwrap();
    visibility::take_calls();
    h.step();
    assert_eq!(visibility::take_calls(), 0);
}
#[test]
fn painted_frame_snapshot_is_not_recomputed_from_later_document_state() {
    let mut h = harness();
    h.state_mut().session.dispatch("perf.start", &json!({})).unwrap();
    h.step();
    let snapshot = h.state().session.perf().report().workload.unwrap();
    h.state_mut().frame_workload = Some(snapshot);
    let canvas = h.state().canvas;
    h.state_mut()
        .session
        .dispatch("node.create", &json!({"canvas":canvas,"id":nodecraft_types::NodeId::from_bytes([99;16]),"kind":"source.text","pos":{"x":0,"y":0}}))
        .unwrap();
    h.state_mut().frame_finished(None);
    assert_eq!(h.state().session.perf().report().workload.unwrap(), snapshot);
    assert_eq!(h.state().session.document().canvases()[&canvas].nodes().len(), snapshot.total_nodes + 1);
}
