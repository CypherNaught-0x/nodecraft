use super::*;
use egui_kittest::Harness;

#[test]
fn a_perf_run_times_each_workload_with_the_pump_and_writes_every_report() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("run.json");
    let outcome = Outcome::default();
    let run = PerfRun::sized(out.clone(), Some(PresentationSource::VsyncFramePump), Some("Test GPU".into()), vec![200, 4], 12, outcome.clone());
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 800.0)).build_ui_state(|ui, run: &mut PerfRun| run.show(ui), run);
    for _ in 0..(2 * 2 * (SETTLE + 12 + 1) + 4) {
        h.step();
    }
    assert!(h.state().done, "{}", h.state().results());
    assert_eq!(outcome.lock().unwrap().clone(), Some(Ok(out.clone())));
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(written, h.state().results());
    assert_eq!(written["complete"], true);
    let runs = written["runs"].as_array().unwrap();
    assert_eq!(
        runs.iter().map(|r| (r["nodes"].as_u64().unwrap(), r["workload"].as_str().unwrap())).collect::<Vec<_>>(),
        [(200, "pan"), (200, "zoom"), (4, "pan"), (4, "zoom")]
    );
    for run in runs {
        let report = &run["report"];
        assert_eq!(report["schema"], "nodecraft.perf/3");
        assert_eq!(report["presentation_source"], "vsync_frame_pump");
        assert_eq!(report["environment"]["renderer"], "Test GPU");
        let presented = report["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "presentation").unwrap();
        // One presented interval per timed frame; the frame that starts the capture follows an
        // unpumped one and isn't counted.
        assert_eq!(presented["samples"], 12, "{presented}");
        assert_eq!(report["workload"]["total_nodes"], run["nodes"]);
        if run["workload"] == "pan" {
            // Panning at 0.2 keeps most of the grid on screen, not the dozen cards of the
            // centred zoom-1 view a freshly opened canvas starts with.
            assert!(report["workload"]["visible_nodes"].as_u64().unwrap() * 2 > run["nodes"].as_u64().unwrap(), "{}", report["workload"]);
        }
    }
}

#[test]
fn the_grid_fixture_is_wired_row_by_row() {
    let (session, canvas) = grid_session(10).unwrap();
    let canvas = session.document().canvases().get(&canvas).unwrap();
    assert_eq!(canvas.nodes().len(), 10);
    // Four columns: three edges in each full row, two in the last.
    assert_eq!(canvas.edges().len(), 3 + 3 + 1);
    assert_eq!(grid_id(usize::MAX), u128::try_from(usize::MAX).unwrap() + 1);
}

#[test]
fn a_run_that_cant_write_or_is_closed_early_says_so() {
    let dir = tempfile::tempdir().unwrap();
    // A directory isn't a file: the write fails and the outcome names the path.
    let outcome = Outcome::default();
    let run = PerfRun::sized(dir.path().to_path_buf(), None, None, vec![4], 2, outcome.clone());
    run.write();
    assert!(matches!(outcome.lock().unwrap().clone(), Some(Err(e)) if e.starts_with(&dir.path().display().to_string())));
    // Closing the window mid-run writes the reports so far, marked incomplete.
    let out = dir.path().join("partial.json");
    let outcome = Outcome::default();
    let mut run = PerfRun::sized(out.clone(), None, None, vec![4], 2, outcome.clone());
    run.runs.push(json!({"nodes": 4, "workload": "pan"}));
    eframe::App::on_exit(&mut run);
    assert_eq!(outcome.lock().unwrap().clone(), Some(Ok(out.clone())));
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!((written["complete"].clone(), written["runs"].as_array().unwrap().len()), (json!(false), 1));
}
