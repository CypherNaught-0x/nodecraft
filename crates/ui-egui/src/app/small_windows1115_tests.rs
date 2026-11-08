//! Small windows (#1115): at the desktop's smallest window, a small laptop window and a full one,
//! the application row fits the window and only moves entries into More from its right, Search
//! and Comments stay in the row, the navigation panel leaves the canvas mostly clear, and the
//! panels and windows opened from the shell stay on the screen.
use super::panel_check::{check_open_panels, check_window};
use super::topbar_tests::reveal;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};

/// The desktop app's smallest window, a small laptop window and its default window.
const SIZES: [Vec2; 3] = [Vec2::new(640.0, 400.0), Vec2::new(800.0, 520.0), Vec2::new(1440.0, 900.0)];

/// The row's entries in the headless shell, in row order.
const ENTRIES: [&str; 11] = ["Sheets", "Techniques", "Elements", "Queue", "Share", "Budgets", "API keys", "ComfyUI", "Plugins", "Search (Ctrl+F)", "Comments"];

fn harness(size: Vec2, kind: crate::theme::ThemeKind) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    app.theme = kind;
    let mut h = Harness::builder().with_size(size).build_ui_state(
        move |ui, app: &mut NodeCraftApp| {
            crate::theme::apply(ui.ctx(), kind);
            app.show(ui);
        },
        app,
    );
    h.run();
    h
}

/// The row's own buttons: every entry it shows, and More.
fn row_buttons(h: &Harness<'static, NodeCraftApp>) -> Vec<(&'static str, Rect)> {
    ENTRIES.iter().chain(["More"].iter()).filter_map(|label| h.query_by_role_and_label(Role::Button, label).map(|node| (*label, node.rect()))).collect()
}

#[test]
fn the_row_fits_and_overflows_from_its_right_at_every_size() {
    for size in SIZES {
        for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
            let h = harness(size, kind);
            let what = format!("{size:?} {kind:?}");
            let bar = h.state().topbar_rect();
            let screen = h.ctx.content_rect();
            assert!(screen.contains_rect(bar), "{what}: the row {bar:?} leaves the window {screen:?}");
            let buttons = row_buttons(&h);
            for (i, (label, rect)) in buttons.iter().enumerate() {
                assert!(bar.contains_rect(*rect), "{what}: {label} {rect:?} overflows the row {bar:?}");
                for (other, theirs) in buttons.iter().skip(i + 1) {
                    assert!(!rect.intersect(*theirs).is_positive(), "{what}: {label} crosses {other}");
                }
            }
            // Search and Comments keep their place in the row from a small laptop window up.
            let shown: Vec<&str> = buttons.iter().map(|(label, _)| *label).collect();
            if size.x >= 800.0 {
                assert!(shown.contains(&"Search (Ctrl+F)") && shown.contains(&"Comments"), "{what}: {shown:?}");
            }
            // Entries leave from the right: what the row keeps of the rest is a prefix of the order.
            let kept: Vec<&str> = ENTRIES.iter().take(9).copied().filter(|label| shown.contains(label)).collect();
            assert_eq!(kept, ENTRIES.iter().take(kept.len()).copied().collect::<Vec<_>>(), "{what}");
            assert_eq!(shown.contains(&"More"), kept.len() < 9, "{what}: More appears only when something has no room");
        }
    }
}

#[test]
fn the_more_menu_holds_only_what_the_row_has_no_room_for() {
    let mut h = harness(Vec2::new(800.0, 520.0), crate::theme::ThemeKind::Dark);
    let row: Vec<&str> = row_buttons(&h).into_iter().map(|(label, _)| label).collect();
    h.get_by_role_and_label(Role::Button, "More").click();
    h.run();
    let menu: Vec<&str> = ENTRIES.iter().copied().filter(|label| !row.contains(label)).collect();
    assert!(!menu.is_empty());
    let screen = h.ctx.content_rect();
    for label in &menu {
        let rect = h.get_by_role_and_label(Role::Button, label).rect();
        assert!(screen.contains_rect(rect), "{label} {rect:?} leaves the window");
    }
    // Each entry is listed once: the row's entries aren't repeated in the menu.
    for label in &row {
        assert_eq!(h.query_all_by_role_and_label(Role::Button, label).count(), 1, "{label}");
    }
}

#[test]
fn an_open_panels_entry_stays_in_the_row() {
    let mut h = harness(Vec2::new(800.0, 520.0), crate::theme::ThemeKind::Dark);
    assert!(h.query_by_role_and_label(Role::Button, "Budgets").is_none(), "Budgets starts in More at this size");
    reveal(&mut h, "Budgets");
    h.get_by_role_and_label(Role::Button, "Budgets").click();
    h.run_steps(12);
    assert!(h.state().budgets.open);
    let budgets = h.get_by_role_and_label(Role::Button, "Budgets");
    assert!(h.state().topbar_rect().contains_rect(budgets.rect()), "the open panel's entry shows in the row");
    for label in ["Search (Ctrl+F)", "Comments"] {
        assert!(h.query_by_role_and_label(Role::Button, label).is_some(), "{label} keeps its place");
    }
}

#[test]
fn the_navigation_panel_leaves_most_of_the_canvas_clear() {
    for size in SIZES {
        let h = harness(size, crate::theme::ThemeKind::Dark);
        let t = h.state().theme.tokens();
        let viewport = h.state().viewport;
        let layout = crate::navigation::layout(viewport, h.state().navigation.visible, t);
        let cover = layout.overlay.intersect(viewport).area() / viewport.area();
        assert!(cover <= 0.25, "{size:?}: the navigation panel covers {:.0}% of the canvas", cover * 100.0);
        assert!(viewport.contains_rect(layout.overlay), "{size:?}");
        // The map hides itself in the smallest window; the zoom badge and Fit stay.
        let small = viewport.width() < t.minimap_hide_width || viewport.height() < t.minimap_hide_height;
        assert_eq!(layout.map_shown, !small, "{size:?}");
        assert_eq!(h.query_by_role_and_label(Role::Button, "Hide map").is_some(), !small, "{size:?}");
        let mut cells = Vec::new();
        for label in ["Fit all", "Fit selected", "100%"] {
            let rect = h.get_by_label(label).rect();
            assert!(layout.toolbar.expand(1.0).contains_rect(rect), "{size:?}: {label} {rect:?} leaves the toolbar {:?}", layout.toolbar);
            cells.push(rect);
        }
        for (i, a) in cells.iter().enumerate() {
            for b in cells.iter().skip(i + 1) {
                assert!(!a.intersect(*b).is_positive(), "{size:?}: toolbar buttons cross");
            }
        }
    }
    // The map's size follows the canvas: the full window keeps the default size.
    let t = crate::theme::ThemeKind::Dark.tokens();
    let wide = crate::navigation::layout(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0)), true, t);
    assert!((wide.overlay.width() - t.navigation_width).abs() < 0.5);
    assert!((wide.map.height() - t.minimap_height).abs() < 0.5);
    let small = crate::navigation::layout(Rect::from_min_size(Pos2::ZERO, Vec2::new(784.0, 504.0)), true, t);
    assert!(small.overlay.width() < t.navigation_width && small.overlay.width() >= t.navigation_min_width);
}

#[test]
fn toolbar_labels_fit_their_cells() {
    let toolbar = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(240.0, 30.0));
    let labels = [70.0, 50.0, 90.0];
    let cells = crate::navigation::toolbar_cells(toolbar, &labels);
    assert_eq!(cells.len(), 3);
    for (cell, label) in cells.iter().zip(labels) {
        assert!(cell.width() >= label && toolbar.expand(0.01).contains_rect(*cell));
    }
    assert!((cells.iter().map(|c| c.width()).sum::<f32>() - toolbar.width()).abs() < 0.01);
    // Labels wider than the toolbar share it rather than spill out of it.
    let squeezed = crate::navigation::toolbar_cells(toolbar, &[200.0, 200.0]);
    assert!(squeezed.iter().all(|cell| toolbar.expand(0.01).contains_rect(*cell)));
    assert!(crate::navigation::toolbar_cells(toolbar, &[]).is_empty());
}

#[test]
fn fit_drops_entries_from_the_right_keeping_open_panels_then_search() {
    use topbar::fit;
    let widths = [100.0; 6];
    let groups = [0, 0, 1, 1, 3, 3];
    let none = [false; 6];
    // Everything fits: nothing moves.
    assert_eq!(fit(&widths, &groups, &none, 50.0, 10_000.0, 6.0), vec![true; 6]);
    // A little short: the rightmost non-tail entry goes first.
    let shown = fit(&widths, &groups, &none, 50.0, 610.0, 6.0);
    assert_eq!(shown, vec![true, true, true, false, true, true]);
    // An open panel's entry stays while others go.
    let pinned = [false, false, false, true, false, false];
    assert_eq!(fit(&widths, &groups, &pinned, 50.0, 500.0, 6.0), vec![true, false, false, true, true, true]);
    // With room for only More and one entry, Search stays and Comments goes.
    assert_eq!(fit(&widths, &groups, &pinned, 50.0, 170.0, 6.0), vec![false, false, false, false, true, false]);
    // With no room at all, More holds everything.
    assert_eq!(fit(&widths, &groups, &none, 50.0, 10.0, 6.0), vec![false; 6]);
    // Hostile widths never panic.
    let _ = fit(&[f32::NAN, f32::INFINITY], &[0], &[], 50.0, f32::NAN, 6.0);
}

#[test]
fn panels_and_windows_stay_on_screen_at_every_size() {
    for size in SIZES {
        let mut h = harness(size, crate::theme::ThemeKind::Dark);
        for label in ["Techniques", "Budgets", "API keys"] {
            reveal(&mut h, label);
            h.get_by_role_and_label(Role::Button, label).click();
            h.run_steps(12);
            check_open_panels(&h, &format!("{label} at {size:?}"));
            reveal(&mut h, label);
            h.get_by_role_and_label(Role::Button, label).click();
            h.run_steps(12);
        }
        h.get_by_label("File").click();
        h.run_steps(4);
        check_window(&h, egui::Id::new("project_files407"), &format!("Project files at {size:?}"));
    }
}
