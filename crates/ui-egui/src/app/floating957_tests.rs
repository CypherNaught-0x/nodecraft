//! Floating panels (#957): real pointer events through the app for stacking, moving by the
//! header and clamping after a window resize.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};

fn harness(width: f32, height: f32) -> Harness<'static, NodeCraftApp> {
    let app = NodeCraftApp::headless().unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(width, height)).with_max_steps(16).build_ui_state(
        move |ui, app: &mut NodeCraftApp| {
            crate::theme::apply(ui.ctx(), app.theme);
            app.show(ui);
        },
        app,
    );
    h.run();
    h
}

fn keys() -> egui::Id {
    egui::Id::new("nodecraft.keys")
}

fn budgets() -> egui::Id {
    egui::Id::new("nodecraft.budgets")
}

fn rect(h: &Harness<'static, NodeCraftApp>, id: egui::Id) -> Rect {
    egui::AreaState::load(&h.ctx, id).unwrap().rect()
}

fn on_top(h: &Harness<'static, NodeCraftApp>, pos: Pos2) -> Option<egui::LayerId> {
    h.ctx.memory(|m| m.layer_id_at(pos))
}

/// Clicks a top-bar entry the way a person does, through More when the row is collapsed.
fn open(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    super::topbar_tests::reveal(h, label);
    h.get_by_role_and_label(Role::Button, label).click();
    h.run();
}

/// A point on a panel's title, away from its close button.
fn header(h: &Harness<'static, NodeCraftApp>, id: egui::Id) -> Pos2 {
    let t = h.state().theme.tokens();
    let rect = rect(h, id);
    Pos2::new(rect.left() + t.card_padding + t.control_gap, rect.top() + t.card_padding + t.control_gap)
}

fn click_at(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.drag_at(pos);
    h.run();
    h.drop_at(pos);
    h.run();
}

fn drag(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, by: Vec2) {
    h.drag_at(from);
    h.run();
    h.hover_at(from + by * 0.5);
    h.run();
    h.hover_at(from + by);
    h.run();
    h.drop_at(from + by);
    h.run();
}

#[test]
fn two_panels_and_a_menu_stack_predictably_and_a_panel_moves_by_its_header() {
    let mut h = harness(480.0, 760.0);
    let document = h.state().session.document().clone();
    open(&mut h, "Budgets");
    open(&mut h, "API keys");
    let t = *h.state().theme.tokens();
    let (b, k) = (rect(&h, budgets()), rect(&h, keys()));
    let row = h.state().topbar_rect();

    // Both open below the row that opened them, the newer one stepped below the older one's header.
    for panel in [b, k] {
        assert!(panel.top() > row.bottom(), "{panel:?} opens below the application row {row:?}");
        assert!(h.ctx.content_rect().contains_rect(panel), "{panel:?} opens inside the window");
    }
    assert!(k.top() >= b.top() + t.panel_cascade - 1.0, "the second panel {k:?} keeps the first one's header {b:?} in view");
    // The last opened is in front; each panel's close button is still the topmost thing under it.
    let overlap = b.intersect(k);
    assert!(overlap.is_positive(), "the two panels overlap in this narrow window, so order matters");
    assert_eq!(on_top(&h, overlap.center()), Some(crate::floating::layer(keys())));
    let closes: Vec<Rect> = h.get_all_by_role_and_label(Role::Button, "Close window").map(|node| node.rect()).collect();
    assert_eq!(closes.len(), 2);
    for close in &closes {
        let layer = on_top(&h, close.center()).unwrap();
        assert_eq!(layer.order, crate::floating::PANEL_ORDER, "nothing covers a close button");
    }

    // Pressing the panel behind brings it to the front.
    let behind = Pos2::new(b.center().x, b.top() + t.panel_cascade * 0.5 + t.card_padding * 0.5);
    assert_eq!(on_top(&h, behind), Some(crate::floating::layer(budgets())));
    click_at(&mut h, behind);
    assert_eq!(on_top(&h, overlap.center()), Some(crate::floating::layer(budgets())), "the last pressed panel is in front");

    // A menu always sits above panels, whichever was pressed last.
    super::topbar_tests::reveal(&mut h, "Sheets");
    assert!(egui::Popup::is_id_open(&h.ctx, topbar::popup_id()));
    let menu = h.ctx.memory(|m| m.area_rect(topbar::popup_id())).unwrap();
    let shared = menu.intersect(b.union(k));
    assert!(shared.is_positive(), "the menu {menu:?} opens over the panels");
    assert_eq!(on_top(&h, shared.center()).map(|layer| layer.order), Some(egui::Order::Foreground));
    h.key_press(egui::Key::Escape);
    h.run();

    // Dragging the header moves the panel; dragging its body does not.
    let before = rect(&h, keys());
    let from = header(&h, keys());
    drag(&mut h, from, Vec2::new(-4.0, 120.0));
    let moved = rect(&h, keys());
    assert!((moved.min - (before.min + Vec2::new(-4.0, 120.0))).length() < 2.0, "{before:?} moved by its header to {moved:?}");
    let body = moved.center();
    drag(&mut h, body, Vec2::new(0.0, -80.0));
    assert_eq!(rect(&h, keys()).min, moved.min, "dragging inside the body does not move the panel");

    // None of it is a document change.
    assert_eq!(h.state().session.document(), &document);
    assert!(!h.state().session.can_undo());
}

#[test]
fn a_panel_stays_inside_the_window_after_a_resize() {
    let mut h = harness(1600.0, 900.0);
    open(&mut h, "API keys");
    let before = rect(&h, keys());
    // Put it in the bottom-right corner, then shrink the window under it.
    let from = header(&h, keys());
    drag(&mut h, from, Vec2::new(0.0, 900.0 - before.bottom()));
    let parked = rect(&h, keys());
    assert!(parked.bottom() > 700.0, "{parked:?} sits low before the resize");
    for size in [Vec2::new(900.0, 600.0), Vec2::new(520.0, 420.0)] {
        h.set_size(size);
        h.run();
        let now = rect(&h, keys());
        let area = h.state().floating_bounds().area;
        assert!(area.expand(1.0).contains_rect(now), "{now:?} is pulled back into {area:?} at {size:?}");
        assert!(now.top() > h.state().topbar_rect().bottom(), "{now:?} never slides under the application row");
        let close = h.get_by_role_and_label(Role::Button, "Close window").rect();
        assert!(h.ctx.content_rect().contains_rect(close), "its close button stays reachable at {size:?}");
    }
    // The close button still works after the move and both resizes.
    h.get_by_role_and_label(Role::Button, "Close window").click();
    h.run();
    assert!(!h.state().keys.open);
}

#[test]
fn reopening_a_panel_over_an_open_one_keeps_both_headers_in_view() {
    let mut h = harness(1600.0, 900.0);
    let t = *h.state().theme.tokens();
    open(&mut h, "Budgets");
    open(&mut h, "API keys");
    let first = rect(&h, keys());
    let close_keys = h.get_all_by_role_and_label(Role::Button, "Close window").map(|node| node.rect()).find(|close| first.contains_rect(*close)).unwrap();
    click_at(&mut h, close_keys.center());
    assert!(!h.state().keys.open);
    // Budgets is moved to where Keys was; reopening Keys must step below its header again.
    let from = header(&h, budgets());
    let by = first.min - rect(&h, budgets()).min;
    drag(&mut h, from, by);
    open(&mut h, "API keys");
    let (b, k) = (rect(&h, budgets()), rect(&h, keys()));
    assert!(k.top() >= b.top() + t.panel_cascade - 1.0, "{k:?} reopened below the header of {b:?}");
    assert_eq!(on_top(&h, b.intersect(k).center()), Some(crate::floating::layer(keys())));
}

#[test]
fn every_panel_resizes_from_its_bottom_right_corner_and_keeps_its_top_left() {
    let mut h = harness(1600.0, 900.0);
    for (label, id) in [("API keys", keys()), ("Budgets", budgets())] {
        open(&mut h, label);
        // Away from the window's right edge, so the clamp does not move it while it grows.
        let from = header(&h, id);
        drag(&mut h, from, Vec2::new(-400.0, 100.0));
        let before = rect(&h, id);
        drag(&mut h, before.max - Vec2::splat(1.0), Vec2::new(60.0, 80.0));
        let after = rect(&h, id);
        assert!((after.min - before.min).length() < 1.0, "{label}: resizing keeps the top-left {before:?} -> {after:?}");
        assert!((after.width() - before.width() - 60.0).abs() < 2.0, "{label}: wider by the drag {before:?} -> {after:?}");
        assert!(after.height() >= before.height() - 1.0, "{label}: never shorter after the drag {before:?} -> {after:?}");
        let close = h.get_all_by_role_and_label(Role::Button, "Close window").map(|node| node.rect()).find(|close| after.contains_rect(*close)).unwrap();
        click_at(&mut h, close.center());
    }
    // The left edge is no handle: dragging it neither resizes nor moves the panel.
    open(&mut h, "API keys");
    let before = rect(&h, keys());
    drag(&mut h, Pos2::new(before.left() + 1.0, before.center().y), Vec2::new(-60.0, 0.0));
    assert_eq!(rect(&h, keys()), before);
}

#[test]
fn opening_search_moves_a_panel_out_from_under_it_with_its_close_button_reachable() {
    let mut h = harness(1600.0, 900.0);
    open(&mut h, "API keys");
    let label = if cfg!(target_os = "macos") { "Search (⌘F)" } else { "Search (Ctrl+F)" };
    open(&mut h, label);
    assert!(h.state().search.open);
    h.run();
    let search = h.state().search_rect();
    let panel = rect(&h, keys());
    assert!(panel.top() >= search.bottom(), "{panel:?} moved below the open search {search:?}");
    let close = h.get_by_role_and_label(Role::Button, "Close window").rect();
    assert_eq!(on_top(&h, close.center()), Some(crate::floating::layer(keys())), "nothing covers the close button");
    // The search input keeps keyboard focus while it is focused (its own ownership rule); with
    // focus elsewhere, the pointer reaches the close button under the open search.
    h.ctx.memory_mut(|m| m.surrender_focus(h.state().search_input_id()));
    h.run();
    assert!(h.state().search.open);
    click_at(&mut h, close.center());
    assert!(!h.state().keys.open, "and it still closes the panel");
}

#[test]
fn a_first_opening_steps_below_a_moved_panels_header() {
    let mut h = harness(1600.0, 900.0);
    let t = *h.state().theme.tokens();
    open(&mut h, "Budgets");
    let from = header(&h, budgets());
    drag(&mut h, from, Vec2::new(0.0, 150.0));
    open(&mut h, "API keys");
    let (b, k) = (rect(&h, budgets()), rect(&h, keys()));
    let strip = Rect::from_min_max(b.min, Pos2::new(b.right(), b.top() + t.panel_cascade));
    assert!(!k.intersect(strip).is_positive(), "{k:?} opened over the header of {b:?}");
    let close = h
        .get_all_by_role_and_label(Role::Button, "Close window")
        .map(|node| node.rect())
        .find(|close| b.contains_rect(*close) && close.top() < b.top() + t.panel_cascade)
        .unwrap();
    assert_eq!(on_top(&h, close.center()), Some(crate::floating::layer(budgets())), "{close:?} of {b:?} under {k:?}");
}
