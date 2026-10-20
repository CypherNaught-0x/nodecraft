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

fn press(h: &mut Harness<'_, NodeCraftApp>, role: Role, label: &str) {
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
    h.get_by_label("No quarantined charges are waiting for review (1 decision recorded).");
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

/// Reviews and confirms the fixture's group, leaving its charge reconciled and open.
fn reconcile(h: &mut Harness<'static, NodeCraftApp>) {
    h.state_mut().dispatch_palette_command("usage.ownership");
    h.run();
    press(h, Role::Button, "Review");
    press(h, Role::Button, "Preview");
    press(h, Role::Button, "Confirm");
}

#[test]
fn a_person_settles_a_reconciled_charge_at_the_billed_amount() {
    let path = journal();
    let mut h = scene(&path);
    reconcile(&mut h);
    h.get_by_label("Reconciled charges still open");
    press(&mut h, Role::Button, "Resolve");
    // A typed amount the decision can't carry is refused before anything is asked.
    if let Some(resolving) = &mut h.state_mut().ownership.as_mut().unwrap().resolving {
        resolving.amount = "1.0000001".into();
    }
    press(&mut h, Role::Button, "Preview");
    assert!(h.state().ownership.as_ref().unwrap().resolving.as_ref().unwrap().refused.is_some());
    assert!(!confirm_enabled(&h));
    if let Some(resolving) = &mut h.state_mut().ownership.as_mut().unwrap().resolving {
        resolving.amount = "0.000007".into();
    }
    press(&mut h, Role::Button, "Preview");
    h.get_by_label_contains("Settles at");
    press(&mut h, Role::Button, "Confirm");
    assert!(h.state().notice.as_deref().is_some_and(|notice| notice.starts_with("Settled the charge old at")), "{:?}", h.state().notice);
    assert!(h.query_by_label("Reconciled charges still open").is_none());
    let saved = Journal::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved.usage.charge_resolutions().len(), 1, "saved in the run journal");
    drop(h);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn a_person_releases_a_charge_that_was_never_billed() {
    let path = journal();
    let mut h = scene(&path);
    reconcile(&mut h);
    press(&mut h, Role::Button, "Resolve");
    press(&mut h, Role::Button, "Preview");
    // Changing the outcome takes the preview away.
    press(&mut h, Role::RadioButton, "Never billed");
    assert!(!confirm_enabled(&h));
    press(&mut h, Role::Button, "Preview");
    h.get_by_label_contains("Leaves the totals");
    press(&mut h, Role::Button, "Confirm");
    assert!(h.state().notice.as_deref().is_some_and(|notice| notice.starts_with("Released the charge old")), "{:?}", h.state().notice);
    let saved = Journal::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    assert!(saved.usage.resolvable_charges().is_empty());
    drop(h);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn typed_amounts_become_micros_or_nothing() {
    for (typed, micros) in [("0.000007", Some(7)), ("1", Some(1_000_000)), (".5", Some(500_000)), ("2.", Some(2_000_000)), (" 3.25 ", Some(3_250_000))] {
        assert_eq!(super::micros(typed), micros, "{typed}");
    }
    for typed in ["", ".", "-1", "1.0000001", "1e3", "1,5", "١", "99999999999999999999"] {
        assert_eq!(super::micros(typed), None, "{typed}");
    }
    assert_eq!(super::decimal(7), "0.000007");
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

#[test]
#[ignore = "manual public-safe capture; RESOLVE332_RENDER_DIR"]
fn resolve332_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("RESOLVE332_RENDER_DIR").unwrap());
    let path = journal();
    let mut h = scene(&path);
    reconcile(&mut h);
    let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
    shot.run();
    shot.render().unwrap().save(dir.join("resolve332-list.png")).unwrap();
    press(&mut shot, Role::Button, "Resolve");
    press(&mut shot, Role::Button, "Preview");
    shot.render().unwrap().save(dir.join("resolve332-preview.png")).unwrap();
    drop(shot);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
