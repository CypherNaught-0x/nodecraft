//! Every top-bar panel, opened and closed (#959): each one opens as one floating panel inside the
//! window, none of its controls overlaps another, and choosing its entry again closes it. Run at
//! a narrow and a wide window, in both themes. Search and Discussions are pinned chrome rather
//! than floating panels, and are checked as open and closed.
use super::panel_check::overlaps;
use super::topbar_tests::reveal;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};

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

/// Check each opening state's actual main controls, beyond the shared header/geometry.
fn controls(h: &Harness<'static, NodeCraftApp>, label: &str) {
    match label {
        "Sheets" => {
            h.get_by_role_and_label(Role::Button, "New sheet");
            assert!(h.query_all_by_label("Title").next().is_some());
        }
        "Techniques" => {
            for tab in ["Library", "Assets", "History", "Styles"] {
                h.get_by_label(tab);
            }
        }
        "Elements" => {
            h.get_by_role_and_label(Role::Button, "Import…");
        }
        "Budgets" => {
            h.get_by_role_and_label(Role::Button, "Save");
            for cap in ["Per call", "This session", "This project", "In total"] {
                assert!(h.query_all_by_label(cap).next().is_some());
            }
        }
        "API keys" => {
            h.get_by_label("Add or replace a key");
            assert!(h.get_by_role_and_label(Role::Button, "Save").accesskit_node().is_disabled(), "an empty key cannot be saved");
        }
        "ComfyUI" => {
            h.get_by_label("No server yet. Add one, such as a local ComfyUI, then check it.");
            h.get_by_role_and_label(Role::Button, "Add and check");
        }
        "Plugins" => {
            h.get_by_label("Plugin packages aren't available here.");
        }
        _ => panic!("add opening-state checks for {label}"),
    }
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
                let [(id, rect)] = opened.as_slice() else { panic!("{label} at {size:?} {kind:?} opened {} panels", opened.len()) };
                // Inside the panel area: below the top bar, clear of pinned chrome.
                let area = h.state().floating_bounds().area;
                assert!(area.expand(1.0).contains_rect(*rect), "{label} at {size:?} {kind:?}: {rect:?} leaves the panel area {area:?}");
                let crossing = overlaps(&h, *id, *rect);
                assert!(crossing.is_empty(), "{label} at {size:?} {kind:?}:\n{}", crossing.join("\n"));
                controls(&h, label);
                super::panel_check::menu_evidence(&mut h, &format!("panel-{label}-{kind:?}-{}x{}", size.x, size.y));
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
                let [(id, rect)] = opened.as_slice() else { panic!("{label} at {size:?} {kind:?} opened {} panels", opened.len()) };
                // Inside the panel area: below the top bar, clear of pinned chrome.
                let area = h.state().floating_bounds().area;
                assert!(area.expand(1.0).contains_rect(*rect), "{label} at {size:?} {kind:?}: {rect:?} leaves the panel area {area:?}");
                let crossing = overlaps(&h, *id, *rect);
                assert!(crossing.is_empty(), "{label} at {size:?} {kind:?}:\n{}", crossing.join("\n"));
                controls(&h, label);
                super::panel_check::menu_evidence(&mut h, &format!("panel-{label}-{kind:?}-{}x{}", size.x, size.y));
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
        for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
            let mut h = harness(size, kind);
            h.run();
            let search = &crate::shortcut::with_shortcut(&h.ctx, "Search", "Command+F");
            choose(&mut h, search);
            assert!(h.state().search.open, "{size:?}");
            super::panel_check::menu_evidence(&mut h, &format!("search-{kind:?}-{}x{}", size.x, size.y));
            h.key_press(egui::Key::Escape);
            h.run();
            assert!(!h.state().search.open, "Escape closes Search at {size:?}");
            choose(&mut h, "Comments");
            assert!(h.state().comments.open, "{size:?}");
            super::panel_check::menu_evidence(&mut h, &format!("comments-{kind:?}-{}x{}", size.x, size.y));
            choose(&mut h, "Comments");
            assert!(!h.state().comments.open, "choosing Comments again closes it at {size:?}");
        }
    }
}
