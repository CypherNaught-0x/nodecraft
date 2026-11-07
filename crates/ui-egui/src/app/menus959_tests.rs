//! Every top-bar panel, opened and closed (#959): each one opens as one floating panel inside the
//! window, none of its controls overlaps another, and choosing its entry again closes it. Run at
//! a narrow and a wide window, in both themes. Search and Discussions are pinned chrome rather
//! than floating panels, and are checked as open and closed.
use super::topbar_tests::reveal;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

/// The window sizes checked: the narrowest the overflow menu is built for, and a wide one.
const SIZES: [Vec2; 2] = [Vec2::new(1000.0, 700.0), Vec2::new(1600.0, 900.0)];

/// Each top-bar entry that opens a floating panel, by its label.
const PANELS: &[&str] = &["Sheets", "Techniques", "Elements", "Budgets", "API keys"];

/// Panels whose entry is disabled in a window without their service (ComfyUI servers, the plugin
/// store: the top-bar test checks that), opened here through the same state their entry toggles.
/// The Queue and Share panels need a run host and a relay; their own tests open them.
/// Opens or closes one panel.
type Toggle = fn(&mut NodeCraftApp);

const STATE_PANELS: &[(&str, Toggle)] =
    &[("ComfyUI", |app| app.comfy_server.open = !app.comfy_server.open), ("Plugins", |app| app.plugin_panel.open = !app.plugin_panel.open)];

fn harness(size: Vec2, kind: crate::theme::ThemeKind) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    app.theme = kind;
    Harness::builder().with_size(size).build_ui_state(
        move |ui, app: &mut NodeCraftApp| {
            crate::theme::apply(ui.ctx(), kind);
            app.show(ui);
        },
        app,
    )
}

/// Chooses the top-bar entry `label`, through the overflow menu when it is folded away.
fn choose(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    reveal(h, label);
    h.get_by_role_and_label(Role::Button, label).click();
    // Some panels lay a grid out over two frames and ask for more: step rather than settle.
    h.run_steps(12);
}

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
fn overlaps(h: &Harness<'static, NodeCraftApp>, panel: Rect) -> Vec<String> {
    // The panel's window node: the window whose box is the panel's.
    let mut stack = vec![h.root()];
    let mut window = None;
    while let Some(node) = stack.pop() {
        if node.accesskit_node().role() == Role::Window
            && bounds(&node).is_some_and(|b| (b.min - panel.min).length() < 2.0 && (b.max - panel.max).length() < 2.0)
        {
            window = Some(node);
            break;
        }
        stack.extend(node.children());
    }
    let Some(window) = window else { return vec![format!("no window node for the panel at {panel:?}")] };
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

#[test]
fn every_top_bar_panel_opens_inside_the_window_without_overlaps_and_closes() {
    for size in SIZES {
        for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
            let mut h = harness(size, kind);
            h.run();
            for label in PANELS {
                let before: Vec<egui::Id> = crate::floating::shown(&h.ctx).into_iter().map(|(id, _)| id).collect();
                choose(&mut h, label);
                let opened: Vec<(egui::Id, Rect)> = crate::floating::shown(&h.ctx).into_iter().filter(|(id, _)| !before.contains(id)).collect();
                let [(_, rect)] = opened.as_slice() else { panic!("{label} at {size:?} {kind:?} opened {} panels", opened.len()) };
                // Inside the panel area: below the top bar, clear of pinned chrome.
                let area = h.state().floating_bounds().area;
                assert!(area.expand(1.0).contains_rect(*rect), "{label} at {size:?} {kind:?}: {rect:?} leaves the panel area {area:?}");
                let crossing = overlaps(&h, *rect);
                assert!(crossing.is_empty(), "{label} at {size:?} {kind:?}:\n{}", crossing.join("\n"));
                choose(&mut h, label);
                h.run_steps(2);
                let still: Vec<egui::Id> = crate::floating::shown(&h.ctx).into_iter().map(|(id, _)| id).collect();
                assert!(opened.iter().all(|(id, _)| !still.contains(id)), "{label} at {size:?} {kind:?} stays open after its entry is chosen again");
            }
        }
    }
}

#[test]
fn panels_without_their_service_open_inside_the_window_without_overlaps_and_close() {
    for size in SIZES {
        for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
            let mut h = harness(size, kind);
            h.run();
            for (label, toggle) in STATE_PANELS {
                let before: Vec<egui::Id> = crate::floating::shown(&h.ctx).into_iter().map(|(id, _)| id).collect();
                toggle(h.state_mut());
                h.run_steps(12);
                let opened: Vec<(egui::Id, Rect)> = crate::floating::shown(&h.ctx).into_iter().filter(|(id, _)| !before.contains(id)).collect();
                let [(_, rect)] = opened.as_slice() else { panic!("{label} at {size:?} {kind:?} opened {} panels", opened.len()) };
                // Inside the panel area: below the top bar, clear of pinned chrome.
                let area = h.state().floating_bounds().area;
                assert!(area.expand(1.0).contains_rect(*rect), "{label} at {size:?} {kind:?}: {rect:?} leaves the panel area {area:?}");
                let crossing = overlaps(&h, *rect);
                assert!(crossing.is_empty(), "{label} at {size:?} {kind:?}:\n{}", crossing.join("\n"));
                toggle(h.state_mut());
                h.run_steps(4);
                let still: Vec<egui::Id> = crate::floating::shown(&h.ctx).into_iter().map(|(id, _)| id).collect();
                assert!(opened.iter().all(|(id, _)| !still.contains(id)), "{label} at {size:?} {kind:?} stays open");
            }
        }
    }
}

#[test]
fn search_and_discussions_open_and_close_from_the_top_bar() {
    for size in SIZES {
        let mut h = harness(size, crate::theme::ThemeKind::Dark);
        h.run();
        let search = if cfg!(target_os = "macos") { "Search (⌘F)" } else { "Search (Ctrl+F)" };
        choose(&mut h, search);
        assert!(h.state().search.open, "{size:?}");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().search.open, "Escape closes Search at {size:?}");
        choose(&mut h, "Comments");
        assert!(h.state().comments.open, "{size:?}");
        choose(&mut h, "Comments");
        assert!(!h.state().comments.open, "choosing Comments again closes it at {size:?}");
    }
}
