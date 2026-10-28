//! Top-bar pointer, accessibility and real GPU evidence (#956).
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

fn search_label() -> &'static str {
    if cfg!(target_os = "macos") { "Search (⌘F)" } else { "Search (Ctrl+F)" }
}

pub(super) fn reveal(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    if (matches!(
        label,
        "API keys" | "Techniques" | "Elements" | "ComfyUI" | "Providers" | "Share" | "Queue" | "Budgets" | "Sheets" | "Comments" | "Open as app"
    ) || label.starts_with("Search ("))
        && h.query_by_role_and_label(Role::Button, label).is_none()
        && h.query_by_role_and_label(Role::Button, "More").is_some()
    {
        h.get_by_role_and_label(Role::Button, "More").click();
        h.run();
    }
    if egui::Popup::is_id_open(&h.ctx, topbar::popup_id()) {
        if let Some(node) = h.query_by_role_and_label(Role::Button, label) {
            node.scroll_to_me();
        }
        h.run();
    }
}

fn expected() -> Vec<(topbar::Entry, &'static str)> {
    use topbar::Entry::*;
    vec![
        (Sheets, "Sheets"),
        (Techniques, "Techniques"),
        (Elements, "Elements"),
        (Queue, "Queue"),
        (Share, "Share"),
        (Budgets, "Budgets"),
        (Keys, "API keys"),
        (Comfy, "ComfyUI"),
        (Plugins, "Plugins"),
        (Search, search_label()),
        (Comments, "Comments"),
    ]
}

fn check_icon(h: &Harness<'static, NodeCraftApp>, entry: topbar::Entry, rect: Rect) {
    let images: Vec<_> = h
        .output()
        .shapes
        .iter()
        .filter_map(|clipped| {
            let shape = &clipped.shape;
            let bounds = shape.visual_bounding_rect();
            (shape.texture_id() != egui::TextureId::default() && rect.contains_rect(bounds)).then_some((shape.texture_id(), bounds))
        })
        .collect();
    assert_eq!(images.len(), 1, "one visible image inside {entry:?}");
    let (texture_id, _) = images[0];
    let image = entry.icon().image(&h.ctx, h.state().theme.tokens().icon_size, h.state().theme.tokens());
    let source = image.source(&h.ctx);
    let manager = h.ctx.tex_manager();
    let textures = manager.read();
    let actual = textures.meta(texture_id).unwrap();
    assert_eq!(Some(actual.name.as_str()), source.uri(), "{entry:?} draws its declared Tabler icon at egui's requested raster size");
}

fn harness(width: f32, kind: crate::theme::ThemeKind) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    app.theme = kind;
    Harness::builder().with_size(Vec2::new(width, 700.0)).build_ui_state(
        move |ui, app: &mut NodeCraftApp| {
            crate::theme::apply(ui.ctx(), kind);
            app.show(ui);
        },
        app,
    )
}

#[test]
fn all_topbar_entries_are_labelled_and_ordered() {
    for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = harness(1600.0, kind);
        h.run();
        let mut right = 0.0;
        for (entry, label) in expected() {
            let rect = h.get_by_role_and_label(Role::Button, label).rect();
            assert!(rect.left() >= right, "{label} follows the previous entry");
            assert!(h.state().topbar_rect().contains_rect(rect));
            assert!((rect.height() - kind.tokens().navigation_toolbar_height).abs() < 1.0);
            check_icon(&h, entry, rect);
            right = rect.right();
        }
    }
}

#[test]
fn overflow_preserves_icons_order_disabled_states_and_escape() {
    for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = harness(480.0, kind);
        let document = h.state().session.document().clone();
        assert!(h.query_by_role_and_label(Role::Button, "Sheets").is_none());
        assert!(h.state().viewport.contains_rect(h.get_by_role_and_label(Role::Button, "More").rect()));
        reveal(&mut h, "Sheets");
        let mut bottom = 0.0;
        for (entry, label) in expected() {
            let node = h.get_by_role_and_label(Role::Button, label);
            let rect = node.rect();
            assert!(rect.top() >= bottom, "{label} follows the previous menu row");
            assert!(h.state().viewport.contains_rect(rect));
            check_icon(&h, entry, rect);
            if matches!(entry, topbar::Entry::Queue | topbar::Entry::Share | topbar::Entry::Comfy | topbar::Entry::Plugins) {
                assert!(node.accesskit_node().is_disabled(), "unavailable {entry:?} is visibly disabled");
            }
            bottom = rect.bottom();
        }
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!egui::Popup::is_id_open(&h.ctx, topbar::popup_id()));
        assert_eq!(h.state().session.document(), &document);
        assert!(!h.state().session.can_undo());
    }
}

#[test]
fn overflow_search_and_comments_dispatch_and_keep_their_first_frame_focus() {
    for label in [search_label(), "Comments"] {
        let mut h = harness(480.0, crate::theme::ThemeKind::Dark);
        let document = h.state().session.document().clone();
        let view = h.state().view;
        reveal(&mut h, label);
        h.get_by_role_and_label(Role::Button, label).click();
        h.run();
        assert!(!egui::Popup::is_id_open(&h.ctx, topbar::popup_id()));
        if label == "Comments" {
            assert!(h.state().comments.open, "notice={:?}", h.state().notice);
        } else {
            assert!(h.state().search.open, "notice={:?}", h.state().notice);
            assert_eq!(h.ctx.memory(|m| m.focused()), Some(h.state().search_input_id()));
        }
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().view, view);
    }
}

#[test]
fn tab_focus_follows_the_wide_visual_order_and_skips_disabled_entries() {
    let mut h = harness(1600.0, crate::theme::ThemeKind::Dark);
    let labels: Vec<_> = expected()
        .into_iter()
        .filter_map(|(_, label)| (!h.get_by_role_and_label(Role::Button, label).accesskit_node().is_disabled()).then_some(label))
        .collect();
    for label in labels {
        h.key_press(egui::Key::Tab);
        h.run();
        assert!(h.get_by_role_and_label(Role::Button, label).accesskit_node().is_focused(), "Tab focuses {label}");
    }
}

#[test]
fn overflow_cannot_blur_an_uncommitted_editor() {
    let mut h = harness(480.0, crate::theme::ThemeKind::Dark);
    let document = h.state().session.document().clone();
    h.get_by_role_and_label(Role::TextInput, "Text").click();
    h.run();
    h.event(egui::Event::Text("Still editing".into()));
    h.run();
    let focus = h.ctx.memory(|m| m.focused());
    assert!(h.get_by_role_and_label(Role::Button, "More").accesskit_node().is_disabled());
    h.get_by_role_and_label(Role::Button, "More").click();
    h.run();
    assert_eq!(h.ctx.memory(|m| m.focused()), focus);
    assert!(!egui::Popup::is_id_open(&h.ctx, topbar::popup_id()));
    assert_eq!(h.state().session.document(), &document);
}

#[test]
fn overflow_keyboard_focus_follows_its_visible_row_order() {
    let mut h = harness(480.0, crate::theme::ThemeKind::Dark);
    reveal(&mut h, "Sheets");
    let labels: Vec<_> = expected()
        .into_iter()
        .filter_map(|(_, label)| (!h.get_by_role_and_label(Role::Button, label).accesskit_node().is_disabled()).then_some(label))
        .collect();
    h.get_by_role_and_label(Role::Button, "Sheets").focus();
    h.run();
    for label in labels.into_iter().skip(1) {
        h.key_press(egui::Key::Tab);
        h.run();
        assert!(h.get_by_role_and_label(Role::Button, label).accesskit_node().is_focused(), "menu Tab focuses {label}");
    }
    assert!(!h.state().session.can_undo());
}

#[test]
fn growing_the_viewport_closes_the_overflow_without_editing() {
    let mut h = harness(480.0, crate::theme::ThemeKind::Dark);
    let document = h.state().session.document().clone();
    reveal(&mut h, "Sheets");
    h.set_size(Vec2::new(1600.0, 700.0));
    h.run();
    assert!(h.query_by_role_and_label(Role::Button, "More").is_none());
    assert!(!egui::Popup::is_id_open(&h.ctx, topbar::popup_id()));
    assert_eq!(h.state().session.document(), &document);
}

#[test]
#[ignore = "manual native GPU capture; requires NODECRAFT_TOPBAR956_RENDER_ROOT"]
fn render_topbar() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_TOPBAR956_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for kind in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for width in [1280.0, 480.0] {
            let mut h = harness(width, kind);
            h.run();
            h.render().unwrap().save(root.join(format!("{kind:?}-{width}.png"))).unwrap();
            if let Some(more) = h.query_by_role_and_label(Role::Button, "More") {
                more.click();
                h.run();
                h.render().unwrap().save(root.join(format!("{kind:?}-{width}-menu.png"))).unwrap();
            }
        }
    }
}
