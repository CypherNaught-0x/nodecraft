//! Small windows (#1115): at the desktop's smallest window, a small laptop window and a full one,
//! the application row fits the window and only moves entries into More from its right, Search
//! and Comments stay in the row, the navigation panel leaves the canvas mostly clear, and the
//! panels and windows opened from the shell stay on the screen.
use super::panel_check::{check_menu, check_open_panels};
use super::topbar_tests::reveal;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

/// The desktop app's smallest window, a small laptop window and its default window.
const SIZES: [Vec2; 3] = [Vec2::new(640.0, 400.0), Vec2::new(800.0, 520.0), Vec2::new(1440.0, 900.0)];

/// The row's entries in the headless shell, in row order: File and Tag filter lead it (#1123).
const ENTRIES: [&str; 13] =
    ["File", "Tag filter", "Sheets", "Techniques", "Elements", "Queue", "Share", "Budgets", "API keys", "ComfyUI", "Plugins", "Search (Ctrl+F)", "Comments"];

/// The entries between Tag filter and Search, which leave the row first, from its right.
const BODY: std::ops::Range<usize> = 2..11;

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
            // File, Search and Comments keep their place in the row at every size here; this shell
            // has no agent sidebar, which at 640 points would leave room for neither Search nor
            // Comments. File is the last entry to leave (#1123).
            let shown: Vec<&str> = buttons.iter().map(|(label, _)| *label).collect();
            assert!(shown.contains(&"File") && shown.contains(&"Search (Ctrl+F)") && shown.contains(&"Comments"), "{what}: {shown:?}");
            // Entries leave from the right: what the row keeps of the body is a prefix of its order,
            // and Tag filter goes only once the body has.
            let body = ENTRIES.get(BODY).unwrap();
            let kept: Vec<&str> = body.iter().copied().filter(|label| shown.contains(label)).collect();
            assert_eq!(kept, body.iter().take(kept.len()).copied().collect::<Vec<_>>(), "{what}");
            assert!(shown.contains(&"Tag filter") || kept.is_empty(), "{what}: Tag filter left before the body {shown:?}");
            let all = shown.contains(&"Tag filter") && kept.len() == body.len();
            assert_eq!(shown.contains(&"More"), !all, "{what}: More appears only when something has no room");
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
    // Ranks as `Entry::rank` gives them: the body 0, an open panel's entry 1, Search and Comments 3.
    let none = [0, 0, 0, 0, 3, 3];
    // Everything fits: nothing moves.
    assert_eq!(fit(&widths, &groups, &none, 50.0, 10_000.0, 6.0), vec![true; 6]);
    // A little short: the rightmost non-tail entry goes first.
    let shown = fit(&widths, &groups, &none, 50.0, 610.0, 6.0);
    assert_eq!(shown, vec![true, true, true, false, true, true]);
    // An open panel's entry stays while others go.
    let pinned = [0, 0, 0, 1, 3, 3];
    assert_eq!(fit(&widths, &groups, &pinned, 50.0, 500.0, 6.0), vec![true, false, false, true, true, true]);
    // With room for only More and one entry, Search stays and Comments goes.
    assert_eq!(fit(&widths, &groups, &pinned, 50.0, 170.0, 6.0), vec![false, false, false, false, true, false]);
    // With no room at all, More holds everything.
    assert_eq!(fit(&widths, &groups, &none, 50.0, 10.0, 6.0), vec![false; 6]);
    // Hostile widths never panic.
    let _ = fit(&[f32::NAN, f32::INFINITY], &[0], &[], 50.0, f32::NAN, 6.0);
}

/// #1123: File and Tag filter lead the row. Tag filter leaves after the body and open panels but
/// before Comments and Search; File leaves last of all.
#[test]
fn fit1123_file_stays_longest_and_tag_filter_goes_before_search() {
    use topbar::{Entry, fit};
    let entries = [Entry::File, Entry::Tags, Entry::Sheets, Entry::Budgets, Entry::Search, Entry::Comments];
    let ranks: Vec<u8> = entries.iter().map(|entry| entry.rank(*entry == Entry::Budgets)).collect();
    assert_eq!(ranks, vec![4, 2, 0, 1, 3, 3]);
    let widths = [100.0; 6];
    let groups = [4, 4, 0, 1, 3, 3];
    // Room for More and four entries: Sheets then Budgets (an open panel) go.
    assert_eq!(fit(&widths, &groups, &ranks, 50.0, 520.0, 6.0), vec![true, true, false, false, true, true]);
    // Room for More and three: Tag filter goes next.
    assert_eq!(fit(&widths, &groups, &ranks, 50.0, 420.0, 6.0), vec![true, false, false, false, true, true]);
    // Room for More and one: only File stays.
    assert_eq!(fit(&widths, &groups, &ranks, 50.0, 170.0, 6.0), vec![true, false, false, false, false, false]);
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
        check_menu(&h, topbar::file_popup_id(), &format!("File menu at {size:?}"));
        h.key_press(egui::Key::Escape);
        h.run();
    }
}

/// #1123: File, the top bar's first entry, opens the File menu below it at 640 × 400; Open recent
/// lists the recent projects; Escape closes the menu and doesn't also clear the canvas selection.
#[test]
fn file1123_menu_opens_below_file_lists_recent_and_escape_closes_only_it() {
    let mut app = NodeCraftApp::headless().unwrap();
    // Absolute on every platform; the files needn't exist to be listed.
    for name in ["Public forest.ncraft", "Public river.ncraft"] {
        app.files.projects.remember(std::env::temp_dir().join(name).to_str().unwrap());
    }
    let mut h = Harness::builder().with_size(SIZES[0]).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            crate::theme::apply(ui.ctx(), crate::theme::ThemeKind::Dark);
            app.show(ui);
        },
        app,
    );
    h.run();
    let node = *h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().keys().next().unwrap();
    h.state_mut().selection.only(node);
    let file = h.get_by_role_and_label(Role::Button, "File").rect();
    let bar = h.state().topbar_rect();
    assert!(bar.contains_rect(file) && file.left() - bar.left() < 20.0, "File leads the row: {file:?} {bar:?}");
    h.get_by_role_and_label(Role::Button, "File").click();
    h.run_steps(4);
    assert!(h.state().files.menu_open());
    check_menu(&h, topbar::file_popup_id(), "File menu at 640 × 400");
    let open = h.get_by_label("Open… (Ctrl+O)").rect();
    assert!(open.top() >= file.bottom() - 1.0, "it opens below File: {open:?} {file:?}");
    for label in ["Save (Ctrl+S)", "Save as… (Ctrl+Shift+S)", "Save workflow results…", "Import folder…"] {
        h.get_by_label(label);
    }
    h.get_by_label("Open recent").click();
    h.run_steps(4);
    let screen = h.ctx.content_rect();
    for label in ["Public river.ncraft", "Public forest.ncraft", "Clear recent projects"] {
        let rect = h.get_by_label(label).rect();
        assert!(screen.contains_rect(rect), "{label} {rect:?} leaves the window");
    }
    assert!(!h.get_by_label("Clear recent projects").accesskit_node().is_disabled());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().files.menu_open());
    assert!(h.query_by_label("Open… (Ctrl+O)").is_none() && h.query_by_label("Public river.ncraft").is_none());
    assert!(h.state().selection.nodes().contains(&node), "Escape closed the menu without reaching the canvas");
}

/// #1123: the Tag filter entry opens the existing tag panel below the row, and while a filter is
/// active it carries the match count, in its badge and its accessible name.
#[test]
fn tags1123_the_tag_filter_entry_opens_the_panel_and_counts_matches() {
    let mut h = harness(SIZES[2], crate::theme::ThemeKind::Dark);
    let entry = h.get_by_role_and_label(Role::Button, "Tag filter").rect();
    assert!(h.state().topbar_rect().contains_rect(entry));
    h.get_by_role_and_label(Role::Button, "Tag filter").click();
    h.run();
    assert!(h.state().tag_filter.open);
    assert!(h.state().tag_rect().top() >= h.state().topbar_rect().bottom(), "the panel opens below the row");
    h.get_by_role_and_label(Role::Label, "Tag filter");
    h.get_by_role_and_label(Role::TextInput, "Find a tag");
    h.get_by_role_and_label(Role::Button, "Close tags");
    // The entry closes it again.
    h.get_by_role_and_label(Role::Button, "Tag filter").click();
    h.run();
    assert!(!h.state().tag_filter.open);
    // An active filter: every untagged card matches.
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
    h.run();
    assert!(h.state().tag_filter.active());
    let matches = h.state().tag_filter.results.len();
    assert!(matches > 0);
    let name = format!("Tag filter ({matches} matches)");
    let badged = h.get_by_role_and_label(Role::Button, &name).rect();
    assert!(badged.width() > entry.width(), "the badge widens the entry");
}

/// #1116: the Shared parameters panel stays pinned on the screen at 640 × 400; it closes when
/// Escape clears the selection it edits.
#[test]
fn dialogs1116_shared_parameters_stay_on_screen_and_go_with_the_selection() {
    let mut h = harness(SIZES[0], crate::theme::ThemeKind::Dark);
    let canvas = h.state().canvas;
    let mut nodes = std::collections::BTreeSet::new();
    for n in [201, 202] {
        let node = nodecraft_graph::Node::new(NodeId::from_bytes([n; 16]), "source.text");
        h.state_mut().session.dispatch("node.add", &serde_json::json!({"canvas": canvas, "node": node})).unwrap();
        nodes.insert(node.id);
    }
    h.state_mut().selection.replace(nodes);
    h.run_steps(4);
    let panel = h.get_by_role_and_label(Role::Window, "Shared parameters").rect();
    let screen = h.ctx.content_rect();
    assert!(screen.contains_rect(panel), "{panel:?} leaves the window {screen:?}");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().selection.nodes().is_empty());
    assert!(h.query_by_role_and_label(Role::Window, "Shared parameters").is_none());
}

/// #1123: in a window too narrow even for File, File is in More, and its menu opens below More,
/// on the screen.
#[test]
fn file1123_from_more_opens_below_more() {
    let mut h = harness(Vec2::new(150.0, 400.0), crate::theme::ThemeKind::Dark);
    assert!(h.query_by_role_and_label(Role::Button, "File").is_none(), "File has moved into More");
    let more = h.get_by_role_and_label(Role::Button, "More").rect();
    h.get_by_role_and_label(Role::Button, "More").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "File").click();
    h.run_steps(4);
    assert!(h.state().files.menu_open());
    check_menu(&h, topbar::file_popup_id(), "File menu from More at 150 × 400");
    assert!(h.get_by_label("Open… (Ctrl+O)").rect().top() >= more.bottom(), "it opens below More");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().files.menu_open());
}

/// #1123: with the most recent projects listed (16), at 640 × 400 and at 200 % interface size,
/// the File menu stays below File and on the screen, and Open recent scrolls so Clear recent
/// projects can be reached.
#[test]
fn file1123_a_full_recent_list_scrolls_in_a_short_window() {
    for percent in [100, 200] {
        let mut app = NodeCraftApp::headless().unwrap();
        for n in 0..16 {
            app.files.projects.remember(std::env::temp_dir().join(format!("Public project {n:02}.ncraft")).to_str().unwrap());
        }
        assert_eq!(app.files.projects.recent().len(), 16);
        let mut h = Harness::builder().with_size(SIZES[0]).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        h.run();
        h.state_mut().session.dispatch("view.ui_scale.set", &json!({"percent": percent})).unwrap();
        h.run();
        let what = format!("{percent} %");
        let file = h.get_by_role_and_label(Role::Button, "File").rect();
        h.get_by_role_and_label(Role::Button, "File").click();
        h.run_steps(4);
        check_menu(&h, topbar::file_popup_id(), &format!("File menu at {what}"));
        let menu = egui::AreaState::load(&h.ctx, topbar::file_popup_id()).unwrap().rect();
        assert!(menu.top() >= file.bottom() - 1.0, "{what}: the menu {menu:?} stays below File {file:?}");
        h.get_by_label("Open recent").click();
        h.run_steps(4);
        let screen = h.ctx.content_rect();
        h.get_by_label("Clear recent projects").scroll_to_me();
        h.run_steps(4);
        let clear = h.get_by_label("Clear recent projects").rect();
        assert!(screen.expand(1.0).contains_rect(clear), "{what}: Clear recent projects {clear:?} leaves {screen:?}");
        h.get_by_label("Clear recent projects").click();
        h.run();
        assert!(h.state().files.projects.recent().is_empty(), "{what}: Clear was reached and cleared the list");
    }
}
