//! Held results survive a restart (#614): a result saved beside the run journal is listed in the
//! queue panel on the next launch, and discarding it removes its record. Nothing reads the
//! keychain or calls a provider.
use super::runs::Runs;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::commands::Services;
use nodecraft_engine::held_store::{FileHeld, HeldRecord, HeldStore};
use nodecraft_engine::provider_setup::Readiness;
use nodecraft_providers::{Completed, Output};

const C: CanvasId = CanvasId::from_bytes([63; 16]);
const P: ProjectId = ProjectId::from_bytes([64; 16]);

/// A fresh folder holding a held result saved by the last session, where `runs.json` would be.
fn last_session() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("nodecraft-held-{}-{nanos}-{n}", std::process::id()));
    let record = HeldRecord {
        project: P,
        canvas: C,
        node: NodeId::from_bytes([65; 16]),
        provider: "mock".into(),
        model: "mock-text".into(),
        reason: "its card was deleted".into(),
        completed: Completed::new(vec![Output::Text("a saved caption".into())]),
        routes: Vec::new(),
        recipe: None,
    };
    FileHeld::at(dir.join("held")).write(1, &record.to_bytes().unwrap()).unwrap();
    dir.join("runs.json")
}

fn scene(path: &std::path::Path) -> Harness<'static, NodeCraftApp> {
    let mut project = Project::new(P, "Held").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut services = Services::default();
    let mut runs = Runs::with_maker(&mut services, Box::new(|_, _| Err(Readiness::NotSetUp("not in this test".into())))).unwrap();
    runs.open_journal(path.to_owned());
    let mut session = Session::new(project).unwrap().with_services(services);
    runs.install(&mut session);
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    app.queue_open = true;
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

#[test]
fn a_held_result_from_the_last_session_is_listed_and_can_be_discarded() {
    let path = last_session();
    let dir = path.parent().unwrap().to_owned();
    let mut h = scene(&path);
    h.get_by_label("1 paid result couldn't be kept");
    h.get_by_label("They are kept across restarts until you decide. Nothing here is sent or charged again.");
    h.get_by_label("its card was deleted");
    assert!(h.query_by_label("until quit").is_none(), "a saved result isn't marked");
    if let Ok(shot) = std::env::var("NODECRAFT_HELD_TEST_SHOT") {
        h.render().unwrap().save(shot).unwrap();
    }

    let pos = h.get_by_role_and_label(Role::Button, "Discard").rect().center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.query_by_label("1 paid result couldn't be kept").is_none(), "it is let go");
    assert!(!dir.join("held/1.held").exists(), "and its record removed, so it doesn't come back");
    std::fs::remove_dir_all(dir).unwrap();
}
