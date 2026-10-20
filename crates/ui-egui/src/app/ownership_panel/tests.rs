//! The quarantined-charge review (#362, #384) over a run host whose journal holds a legacy
//! usage log with one quarantined charge. Nothing reads the keychain or calls a provider.
use super::super::runs::Runs;
use super::*;
use crate::palette;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::commands::Services;
use nodecraft_engine::provider_setup::Readiness;
use nodecraft_engine::run_journal::{JOURNAL_VERSION, Journal};
use serde_json::json;

const C: CanvasId = CanvasId::from_bytes([61; 16]);

fn legacy() -> nodecraft_exec::cost::UsageLog {
    serde_json::from_str(include_str!("../../../../exec/src/cost/fixtures/legacy-folded-open-reused-actual.json")).unwrap()
}

/// A run journal holding the legacy log, in a fresh directory the caller removes.
fn journal() -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("nodecraft-ownership-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("runs.json");
    std::fs::write(&path, Journal { version: JOURNAL_VERSION, usage: legacy(), submitted: Vec::new() }.to_bytes().unwrap()).unwrap();
    path
}

fn scene(path: &std::path::Path) -> Harness<'static, NodeCraftApp> {
    let mut project = Project::new(ProjectId::from_bytes([62; 16]), "Ownership").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut services = Services::default();
    let mut runs = Runs::with_maker(&mut services, Box::new(|_, _| Err(Readiness::NotSetUp("not in this test".into())))).unwrap();
    runs.open_journal(path.to_owned());
    let mut session = Session::new(project).unwrap().with_services(services);
    runs.install(&mut session);
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn press(h: &mut Harness<'static, NodeCraftApp>, role: Role, label: &str) {
    let pos = h.get_by_role_and_label(role, label).rect().center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

fn confirm_enabled(h: &Harness<'static, NodeCraftApp>) -> bool {
    !h.get_by_role_and_label(Role::Button, "Confirm").accesskit_node().is_disabled()
}

#[test]
fn a_person_reviews_previews_and_confirms_who_holds_a_quarantined_charge() {
    let path = journal();
    let mut h = scene(&path);
    assert!(h.state().palette_entries().iter().any(|entry| entry.target == palette::Target::Command("usage.ownership")));
    h.state_mut().dispatch_palette_command("usage.ownership");
    h.run();
    h.get_by_label("Session a");
    h.get_by_label("1 charge");
    press(&mut h, Role::Button, "Review");

    // What was known at import, then the charge itself.
    h.get_by_label("The earlier total could hold up to");
    h.get_by_label_contains("t · m");
    assert!(!confirm_enabled(&h), "confirming needs a preview first");
    press(&mut h, Role::Button, "Preview");
    h.get_by_label_contains("held by this session's total");
    assert!(confirm_enabled(&h));
    // Changing the decision takes the preview away.
    press(&mut h, Role::RadioButton, "The earlier total");
    assert!(!confirm_enabled(&h));
    press(&mut h, Role::Button, "Preview");
    press(&mut h, Role::Button, "Confirm");

    assert!(h.state().notice.as_deref().is_some_and(|notice| notice.starts_with("Recorded which total holds 1 charge of session a.")));
    h.get_by_label("No charges are waiting for review (1 decision recorded).");
    // The decision was saved in the run journal before it was kept.
    let saved = Journal::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.usage.ownership_reconciliations().len(), 1);
    drop(h);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_decision_made_stale_meanwhile_is_refused_in_the_panel() {
    let path = journal();
    let mut h = scene(&path);
    h.state_mut().dispatch_palette_command("usage.ownership");
    h.run();
    press(&mut h, Role::Button, "Review");
    press(&mut h, Role::Button, "Preview");
    // Elsewhere (another review), the group is decided first.
    let decision = h.state().ownership.as_ref().unwrap().group.as_ref().unwrap().decision(false);
    let mut other = decision.clone();
    other["holder"] = json!("earlier");
    h.state_mut().session.dispatch("usage.reconcile_ownership", &other).unwrap();

    press(&mut h, Role::Button, "Confirm");
    let refused = h.state().ownership.as_ref().unwrap().group.as_ref().unwrap().refused.clone();
    assert!(refused.is_some(), "a stale decision is refused");
    assert!(!confirm_enabled(&h));
    let saved = Journal::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.usage.ownership_reconciliations().len(), 1, "only the first decision was recorded");
    drop(h);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn without_quarantined_charges_the_review_says_so() {
    let mut h = crate::app::tests::harness();
    // The headless app has no usage log: the palette doesn't offer the review.
    assert!(!h.state().palette_entries().iter().any(|entry| entry.target == palette::Target::Command("usage.ownership")));
    h.state_mut().dispatch_palette_command("usage.ownership");
    h.run();
    assert!(h.state().ownership.is_none());
    assert!(h.state().notice.is_some());
}

#[test]
#[ignore = "manual public-safe capture; OWNERSHIP362_RENDER_DIR"]
fn ownership362_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("OWNERSHIP362_RENDER_DIR").unwrap());
    let path = journal();
    let mut h = scene(&path);
    h.state_mut().dispatch_palette_command("usage.ownership");
    h.run();
    press(&mut h, Role::Button, "Review");
    press(&mut h, Role::Button, "Preview");
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "ownership362-dark.png"), (crate::theme::ThemeKind::Light, "ownership362-light.png")] {
        h.state_mut().theme = theme;
        let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
    drop(h);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
