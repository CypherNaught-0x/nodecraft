//! The shared layout check for floating panels (#959): a panel opens inside the floating panel
//! area, and none of its own on-screen controls crosses another. Panels' own tests call
//! [`check_open_panels`] once they have opened their panel, windows' tests [`check_window`], or
//! [`check_message_window`] for a window that only shows a message.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::NodeT};

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

fn bounds(node: &egui_kittest::Node<'_>) -> Option<Rect> {
    let b = node.accesskit_node().bounding_box()?;
    Some(Rect::from_min_max(Pos2::new(b.x0 as f32, b.y0 as f32), Pos2::new(b.x1 as f32, b.y1 as f32)))
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

/// A window that isn't a floating panel (a dialog, the model picker, Project files): it is open,
/// lies on the screen, and none of its own on-screen controls cross another. `what` names the
/// case in a failure. Call it on a settled harness.
pub(super) fn check_window(h: &Harness<'static, NodeCraftApp>, id: egui::Id, what: &str) {
    window(h, id, what, true);
}

/// [`check_window`] for a window that only says something, with nothing to act on: its window
/// node must still be on screen, but it may hold no controls.
pub(super) fn check_message_window(h: &Harness<'static, NodeCraftApp>, id: egui::Id, what: &str) {
    window(h, id, what, false);
}

fn window(h: &Harness<'static, NodeCraftApp>, id: egui::Id, what: &str, controls: bool) {
    let Some(state) = egui::AreaState::load(&h.ctx, id) else { panic!("{what}: the window {id:?} isn't open") };
    let rect = state.rect();
    let screen = h.ctx.content_rect();
    assert!(rect.is_finite() && rect.is_positive(), "{what}: {rect:?} has no size");
    assert!(screen.expand(1.0).contains_rect(rect), "{what}: {rect:?} leaves the window {screen:?}");
    let mut crossing = overlaps(h, id, rect);
    if !controls {
        crossing.retain(|found| !found.starts_with("no controls found"));
    }
    assert!(crossing.is_empty(), "{what}:\n{}", crossing.join("\n"));
}
