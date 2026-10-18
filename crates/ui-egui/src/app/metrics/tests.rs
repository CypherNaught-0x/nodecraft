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
