//! The shared layout check for floating panels (#959): a panel opens inside the floating panel
//! area, and none of its own on-screen controls crosses another. Panels' own tests call
//! [`check_open_panels`] once they have opened their panel, and questions' tests (the
//! `floating::question` modals, #1116) [`check_question`].
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

/// Controls a person acts on.
fn interactive(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::CheckBox
            | Role::ComboBox
            | Role::Slider
            | Role::SpinButton
            | Role::RadioButton
            | Role::Link
    )
}

/// The node's rect in points, for nodes that have bounds. AccessKit's bounding boxes are in
/// physical pixels, which differ from points at an interface size other than 100 % (#1107) or on
/// a high-density screen; `Node::rect` converts them.
fn bounds(node: &egui_kittest::Node<'_>) -> Option<Rect> {
    node.accesskit_node().bounding_box().map(|_| node.rect())
}

/// Pairs of the panel's own on-screen controls that cross; a control may share space only with
/// its own accessibility descendants. Clipping isn't checked: egui's tree doesn't mark scroll
/// areas, so content scrolled out of view can't be told from content cut off.
pub(super) fn overlaps(h: &Harness<'static, NodeCraftApp>, id: egui::Id, panel: Rect) -> Vec<String> {
    // The panel's window node is its area's move response, `<id>.with("move")` in egui 0.36:
    // matched by id, so a neighbouring panel or a modal can never stand in for it.
    let wanted = format!("{:?})", id.with("move").accesskit_id());
    let mut stack = vec![h.root()];
    let mut window = None;
    while let Some(node) = stack.pop() {
        if format!("{:?}", node.accesskit_node().id()).ends_with(&wanted) {
            window = Some(node);
            break;
        }
        stack.extend(node.children());
    }
    let Some(window) = window else { return vec![format!("no window node for the panel {id:?} at {panel:?}")] };
    let mut controls: Vec<(String, Rect, String, Vec<String>)> = Vec::new();
    let mut found = Vec::new();
    let mut stack = vec![(window, Vec::<String>::new())];
    while let Some((node, ancestors)) = stack.pop() {
        let id = format!("{:?}", node.accesskit_node().id());
        let role = node.accesskit_node().role();
        let mut below = ancestors.clone();
        below.push(id.clone());
        for child in node.children() {
            stack.push((child, below.clone()));
        }
        let Some(rect) = bounds(&node) else { continue };
        if !interactive(role) || node.accesskit_node().is_hidden() || !rect.is_positive() {
            continue;
        }
        let name = format!("{role:?} {:?}", node.accesskit_node().label().unwrap_or_default());
        // What a scroll area hides isn't on screen to cross anything.
        if panel.contains_rect(rect) {
            controls.push((name, rect, id, ancestors));
        }
    }
    // Every panel has at least its close button: none found means nothing was checked.
    if controls.is_empty() {
        found.push(format!("no controls found in the panel {id:?} at {panel:?}"));
    }
    for (i, (a, ra, ia, pa)) in controls.iter().enumerate() {
        for (b, rb, ib, pb) in controls.iter().skip(i + 1) {
            let related = pa.contains(ib) || pb.contains(ia);
            let both = ra.intersect(*rb);
            if !related && both.is_positive() && both.area() > 4.0 {
                found.push(format!("{a} {ra:?} overlaps {b} {rb:?}"));
            }
        }
    }
    found
}

/// Every floating panel open in `h` sits in the floating panel area with no crossing controls;
/// `what` names the case in a failure. Call it on a settled harness (after `run`), so no panel
/// that closed a frame ago is still listed.
pub(super) fn check_open_panels(h: &Harness<'static, NodeCraftApp>, what: &str) {
    let area = h.state().floating_bounds().area;
    let shown = crate::floating::shown(&h.ctx);
    assert!(!shown.is_empty(), "{what}: no floating panel is open");
    for (id, rect) in shown {
        assert!(area.expand(1.0).contains_rect(rect), "{what}: {rect:?} leaves the panel area {area:?}");
        let crossing = overlaps(h, id, rect);
        assert!(crossing.is_empty(), "{what}:\n{}", crossing.join("\n"));
    }
}

/// A question (`floating::question`, #1116): it is open, lies on the screen, and none of its own
/// on-screen controls cross another. `what` names the case in a failure. Call it on a settled
/// harness.
pub(super) fn check_question(h: &Harness<'static, NodeCraftApp>, id: egui::Id, what: &str) {
    let Some(state) = egui::AreaState::load(&h.ctx, id) else { panic!("{what}: the question {id:?} isn't open") };
    let rect = state.rect();
    let screen = h.ctx.content_rect();
    assert!(rect.is_finite() && rect.is_positive(), "{what}: {rect:?} has no size");
    assert!(screen.expand(1.0).contains_rect(rect), "{what}: {rect:?} leaves the window {screen:?}");
    let crossing = overlaps(h, id, rect);
    assert!(crossing.is_empty(), "{what}:\n{}", crossing.join("\n"));
}

/// A menu (the File menu, #1123): it is open, lies on the screen, and none of its own on-screen
/// controls cross another. `id` is its popup's id; `what` names the case in a failure. Call it on
/// a settled harness.
pub(super) fn check_menu(h: &Harness<'static, NodeCraftApp>, id: egui::Id, what: &str) {
    let Some(state) = egui::AreaState::load(&h.ctx, id) else { panic!("{what}: the menu {id:?} isn't open") };
    let rect = state.rect();
    let screen = h.ctx.content_rect();
    assert!(rect.is_finite() && rect.is_positive(), "{what}: {rect:?} has no size");
    assert!(screen.expand(1.0).contains_rect(rect), "{what}: {rect:?} leaves the window {screen:?}");
    let crossing = overlaps(h, id, rect);
    assert!(crossing.is_empty(), "{what}:\n{}", crossing.join("\n"));
}

/// Exercise the real shared header control, then prove the panel stopped being drawn.
/// Callers also assert their own open state/lifecycle cleanup after this returns.
pub(super) fn close_panel(h: &mut Harness<'static, NodeCraftApp>, id: egui::Id, what: &str) {
    h.run_steps(4);
    menu_evidence(h, what);
    h.get_by_role_and_label(Role::Button, "Close window").click();
    h.run_steps(4);
    assert!(!crate::floating::shown(&h.ctx).iter().any(|(shown, _)| *shown == id), "{what} still draws after Close window");
}

/// Opt-in public-safe inventory evidence, shared by the real open/control/close flows.
pub(super) fn menu_evidence(h: &mut Harness<'static, NodeCraftApp>, what: &str) {
    if let Some(folder) = std::env::var_os("NODECRAFT_MENU959_EVIDENCE") {
        let folder = std::path::PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        h.render().unwrap().save(folder.join(format!("{what}.png"))).unwrap();
    }
}

/// The tool owns Escape only after the focused editor (if any) has yielded it. The
/// application-level owner tests exercise the first-press/editor/gesture precedence.
pub(super) fn escape_panel(h: &mut Harness<'static, NodeCraftApp>, id: egui::Id, what: &str) {
    h.ctx.memory_mut(|memory| {
        if let Some(focused) = memory.focused() {
            memory.surrender_focus(focused);
        }
    });
    h.run_steps(4);
    menu_evidence(h, what);
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert!(!crate::floating::shown(&h.ctx).iter().any(|(shown, _)| *shown == id), "{what} still draws after its Escape");
}
