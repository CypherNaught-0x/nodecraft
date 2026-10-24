//! The edit a settled canvas cannot skip (#556). `canvas_frame_times` measures the frame after a
//! `node.move`, and a canvas with nothing to recompute answers that without asking the engine
//! anything (#549). This is the other half of the same boundary: `node.param_set` on a node that
//! isn't an action, on the same generated graph, the same zoom and pixels per point, the same
//! timed span (the egui pass with AccessKit plus tessellation, the edit itself untimed just
//! before it). Such an edit changes what an action reads, so the whole `live_actions::pending`
//! path runs on the UI thread — the staleness walk, the input hashing, the reuse index, the
//! planner walk — to find out what, if anything, follows.
//!
//! The number of live looks each loop took is printed with the times, and compared: without that
//! the two lines would be indistinguishable, and a frame that skipped the look would look like a
//! fast one. A frame while a worker is still running doesn't ask, so the counts are compared
//! rather than checked for an exact figure.
use super::*;

#[test]
#[ignore = "release-mode frame-time benchmark for #556; run with --release -- --ignored --nocapture"]
fn frame_after_an_edit_that_cannot_be_skipped() {
    const FRAMES: usize = 60;
    for ppp in [1.0, 2.0] {
        for count in [200, 1000] {
            let mut h = graph(count, ppp);
            settle(&mut h, 0.1);
            let canvas = h.state().canvas;
            // The text source at the head of the first row: not an action, and what the row's
            // relays read.
            let head = NodeId::from_bytes(u128::try_from(1).unwrap().to_be_bytes());
            // Let the first live round land and the next look find nothing: this is the state a
            // move is skipped in and a parameter edit is not.
            for _ in 0..30 {
                h.step();
            }
            let settled = h.state().live.settled();
            let looks = h.state().live.pending_looks;
            // The control, with the same loop `canvas_frame_times` runs: what its "frame after an
            // edit" line really measures.
            let moved = frames(&mut h, FRAMES, |h, frame| {
                let id = NodeId::from_bytes(u128::try_from(frame % 50 + 2).unwrap().to_be_bytes());
                let pos = json!({"x": (frame % 50) as f32 * 400.0, "y": 10.0 * (frame % 3) as f32});
                h.state_mut().session.dispatch("node.move", &json!({"canvas": canvas, "id": id, "pos": pos})).unwrap();
            });
            let move_looks = h.state().live.pending_looks - looks;
            let before_looks = h.state().live.pending_looks;
            let edited = frames(&mut h, FRAMES, |h, frame| {
                let value = Value::Text(format!("Prompt {frame}: a quiet harbour at dusk, soft light through the masts."));
                h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas, "id": head, "name": "text", "value": value})).unwrap();
            });
            let param_looks = h.state().live.pending_looks - before_looks;
            eprintln!("ppp {ppp} {count:>5} nodes  frame after a move at 0.1 (canvas settled: {settled}): {moved}  [{move_looks} live looks]");
            eprintln!("ppp {ppp} {count:>5} nodes  frame after a parameter edit at 0.1: {edited}  [{param_looks} live looks]");
            assert!(param_looks > move_looks, "{count} nodes ppp {ppp}: a parameter edit took {param_looks} live looks, a move {move_looks}");
        }
    }
}
