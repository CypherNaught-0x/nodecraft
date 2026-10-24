//! Deliberate tag definition edits through the existing persistent commands.
use super::*;
use nodecraft_graph::Tag;

pub(super) struct Editor {
    canvas: CanvasId,
    original: Tag,
    title: String,
    deleting: bool,
    identity: Arc<()>,
    focus: bool,
    error: Option<String>,
}

impl Editor {
    pub(super) fn new(canvas: CanvasId, original: Tag, deleting: bool, identity: Arc<()>) -> Self {
        Self { canvas, title: original.title.clone(), original, deleting, identity, focus: true, error: None }
    }
}

/// The exact ID stays visible above the title, independent of sorting and rename.
pub(super) fn row_text(ui: &egui::Ui, tag: &Tag, state: &str, t: &theme::Tokens) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &format!("{}\n", tag.id),
        0.0,
        egui::TextFormat { font_id: egui::FontId::monospace(t.tag_identity_size), color: t.text_muted, ..Default::default() },
    );
    job.append(
        &format!("{}{state}", crate::tags::label(&tag.title)),
        0.0,
        egui::TextFormat { font_id: egui::TextStyle::Body.resolve(ui.style()), color: t.tag_color(tag.color.as_deref()), ..Default::default() },
    );
    // Lay out explicitly: the list's single-line truncation must not hide the identity.
    job.wrap = egui::text::TextWrapping {
        max_width: (ui.available_width() - ui.spacing().icon_width - ui.spacing().icon_spacing - ui.spacing().button_padding.x * 2.0).max(1.0),
        max_rows: 2,
        ..Default::default()
    };
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    galley.into()
}

/// `show_rows` virtualizes using this exact height; natural text height is smaller.
pub(super) fn row(ui: &mut egui::Ui, enabled: bool, widget: impl egui::Widget, t: &theme::Tokens) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| ui.add_sized([ui.available_width(), t.tag_definition_row_height], widget)).inner
}

pub(super) fn match_cue(ui: &mut egui::Ui, painter: &egui::Painter, view: &CanvasView, viewport: Rect, rect: Rect, id: NodeId, t: &theme::Tokens) {
    // Fixed-length inset corners stay on the card, separate from the selected border.
    // Their allocation is bounded by theme lengths, never by a hostile card perimeter.
    let inner = rect.shrink(t.navigation_gap);
    let leg = t.tag_row_height.min(inner.width() * 0.5).min(inner.height() * 0.5);
    if leg <= 0.0 {
        return;
    }
    for (corner, dx, dy) in [(inner.left_top(), 1.0, 1.0), (inner.right_top(), -1.0, 1.0), (inner.right_bottom(), -1.0, -1.0), (inner.left_bottom(), 1.0, -1.0)]
    {
        let path = [corner + Vec2::new(dx * leg, 0.0), corner, corner + Vec2::new(0.0, dy * leg)];
        painter.extend(egui::Shape::dashed_line(&path, egui::Stroke::new(t.handle_stroke, t.accent), t.control_gap, t.control_gap));
    }
    let cue = Rect::from_center_size(inner.left_top(), Vec2::splat(t.tag_marker_width));
    ui.interact(view.rect_to_screen(viewport.min, cue), ui.id().with(("tag_match", id)), Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Tag filter match"));
}

impl NodeCraftApp {
    pub(super) fn tag_definition_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut editor) = self.tag_definition.take() else { return false };
        if editor.canvas != self.canvas || !Arc::ptr_eq(&editor.identity, &self.session.view_identity()) {
            return false;
        }
        let current = self.session.document().canvases().get(&editor.canvas).and_then(|c| c.tags().get(&editor.original.id));
        if current != Some(&editor.original) {
            self.notice = Some("The tag changed or was removed. Open its controls again.".into());
            return false;
        }
        let t = self.theme.tokens();
        let mut submit = false;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::new("tag_definition")).show(ui.ctx(), |ui| {
            ui.set_max_width(t.viewer_width);
            ui.heading(if editor.deleting { "Delete tag?" } else { "Rename tag" });
            ui.label(crate::tags::label(&editor.original.title));
            ui.label(egui::RichText::new(editor.original.id.to_string()).small().color(t.text_muted));
            if editor.deleting {
                ui.label("This removes the tag from every card in this canvas.");
                ui.label("Undo restores the tag and its cards. Select it again to restore its filter choice.");
            } else {
                let response = ui.add(egui::TextEdit::singleline(&mut editor.title).hint_text("Tag name").char_limit(4096));
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Tag name"));
                if editor.focus {
                    response.request_focus();
                    editor.focus = false;
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && editor.title.len() <= 4096 {
                    submit = true;
                }
                if editor.title.len() > 4096 {
                    ui.label("Use a name of at most 4096 UTF-8 bytes.");
                }
            }
            if let Some(error) = &editor.error {
                ui.label(egui::RichText::new(error).color(t.danger));
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(editor.deleting || editor.title.len() <= 4096, egui::Button::new(if editor.deleting { "Delete tag" } else { "Save tag name" }))
                    .clicked()
                {
                    submit = true;
                }
                cancel = ui.button("Cancel tag edit").clicked();
            });
        });
        if submit {
            let result = if editor.deleting {
                self.session.dispatch("tag.remove", &json!({"canvas": editor.canvas, "tag": editor.original.id}))
            } else {
                self.session.dispatch(
                    "tag.put",
                    &json!({"canvas": editor.canvas, "tag": {
                        "id": editor.original.id, "title": editor.title, "color": editor.original.color
                    }}),
                )
            };
            match result {
                Ok(_) => return true,
                Err(error) => editor.error = Some(error.message),
            }
        }
        if !cancel && !modal.should_close() {
            self.tag_definition = Some(editor);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable};
    use nodecraft_types::TagId;

    const TAG: TagId = TagId::from_bytes([17; 16]);

    fn row(title: &str) -> String {
        format!("{TAG}\n{title}")
    }

    fn harness(theme: ThemeKind) -> Harness<'static, NodeCraftApp> {
        harness_with_render(theme, false)
    }

    fn harness_with_render(theme: ThemeKind, render: bool) -> Harness<'static, NodeCraftApp> {
        let mut app = NodeCraftApp::headless().unwrap();
        app.theme = theme;
        let id = *app.session.document().canvases().get(&app.canvas).unwrap().nodes().keys().next().unwrap();
        app.selection.only(id);
        app.session
            .dispatch(
                "node.tag_set",
                &json!({"canvas":app.canvas,"nodes":[id],"tag":TAG,
            "create":{"id":TAG,"title":"Blue","color":"blue"}}),
            )
            .unwrap();
        app.session
            .dispatch(
                "tag.put",
                &json!({"canvas":app.canvas,"tag":{
                    "id":TagId::from_bytes([18;16]),"title":"Yellow","color":"yellow"
                }}),
            )
            .unwrap();
        let builder = Harness::builder().with_size(Vec2::new(1200.0, 800.0)).with_max_steps(160);
        let builder = if render { builder.wgpu() } else { builder };
        let mut h = builder.build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        theme::apply(&h.ctx, theme);
        h.run();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[TAG],"untagged":false})).unwrap();
        h.run();
        h
    }

    fn controls(h: &mut Harness<'static, NodeCraftApp>, title: &str, button: &str) {
        let pos = h.get_by_label(&row(title)).rect().center();
        h.hover_at(pos);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
            h.run();
        }
        h.get_by_role_and_label(Role::Button, button).click();
        h.run();
    }

    fn keyboard_controls(h: &mut Harness<'static, NodeCraftApp>, title: &str) {
        for _ in 0..40 {
            if h.get_by_label(&row(title)).is_focused() {
                break;
            }
            h.key_press(egui::Key::Tab);
            h.run();
        }
        assert!(h.get_by_label(&row(title)).is_focused(), "Tab reaches the tag row");
        h.event(egui::Event::Key { key: egui::Key::F10, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::SHIFT });
        h.run();
        assert!(h.get_by_role_and_label(Role::Button, "Rename tag…").is_focused());
    }

    #[test]
    fn keyboard_rename_enter_and_delete_confirmation_keep_selection_and_undo() {
        for theme in [ThemeKind::Dark, ThemeKind::Light] {
            let mut h = harness(theme);
            let original = h.state().session.document().clone();
            let selected = h.state().selection.clone();
            keyboard_controls(&mut h, "Blue");
            h.key_press(egui::Key::Enter);
            h.run();
            assert!(h.get_by_role_and_label(Role::TextInput, "Tag name").is_focused());
            h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
            h.event(egui::Event::Text("Zinc".into()));
            h.run();
            h.key_press(egui::Key::Enter);
            h.run();
            assert!(h.state().tag_definition.is_none());
            h.get_by_label(&row("Zinc"));
            assert_eq!(h.state().selection, selected);
            keyboard_controls(&mut h, "Zinc");
            h.key_press(egui::Key::Tab);
            h.run();
            assert!(h.get_by_role_and_label(Role::Button, "Delete tag…").is_focused());
            h.key_press(egui::Key::Enter);
            h.run();
            assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().contains_key(&TAG));
            h.key_press(egui::Key::Escape);
            h.run();
            assert!(h.state().tag_definition.is_none());
            keyboard_controls(&mut h, "Zinc");
            h.key_press(egui::Key::Tab);
            h.run();
            h.key_press(egui::Key::Enter);
            h.run();
            for _ in 0..4 {
                if h.get_by_role_and_label(Role::Button, "Delete tag").is_focused() {
                    break;
                }
                h.key_press(egui::Key::Tab);
                h.run();
            }
            assert!(h.get_by_role_and_label(Role::Button, "Delete tag").is_focused());
            h.key_press(egui::Key::Enter);
            h.run();
            assert!(!h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().contains_key(&TAG));
            assert_eq!(h.state().selection, selected);
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &original);
        }
    }

    #[test]
    fn pointer_rename_and_delete_are_deliberate_undoable_and_keep_selection_in_both_themes() {
        for theme in [ThemeKind::Dark, ThemeKind::Light] {
            let mut h = harness(theme);
            let original = h.state().session.document().clone();
            let selected = h.state().selection.clone();
            let revision = h.state().session.revision();
            assert_eq!(h.get_all_by_label("Tag filter match").count(), 1);
            controls(&mut h, "Blue", "Rename tag…");
            assert_eq!(h.state().session.revision(), revision);
            // Replace the actual focused name input with Command+A and text events.
            h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
            h.event(egui::Event::Text("Zinc".into()));
            h.run();
            h.get_by_role_and_label(Role::Button, "Save tag name").click();
            h.run();
            assert_eq!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().get(&TAG).unwrap().title, "Zinc");
            assert_eq!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().get(&TAG).unwrap().color.as_deref(), Some("blue"));
            assert_eq!(h.state().selection, selected);
            let renamed = h.get_by_label(&row("Zinc")).rect();
            let other = h.get_by_label(&format!("{}\nYellow", TagId::from_bytes([18; 16]))).rect();
            assert!(renamed.top() > other.top(), "rename reorders rows while their exact IDs stay stable");
            controls(&mut h, "Zinc", "Delete tag…");
            h.get_by_role_and_label(Role::Button, "Cancel tag edit").click();
            h.run();
            assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().contains_key(&TAG));
            controls(&mut h, "Zinc", "Delete tag…");
            h.get_by_role_and_label(Role::Button, "Delete tag").click();
            h.run();
            assert!(!h.state().tag_filter.active());
            assert_eq!(h.state().selection, selected);
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            h.run();
            assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().contains_key(&TAG));
            assert!(!h.state().tag_filter.active(), "undo restores document state, not the dropped transient filter choice");
            h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
            assert_eq!(h.state().session.document(), &original);
        }
    }

    #[test]
    fn escape_and_external_changes_discard_a_definition_draft_without_writing_it() {
        let mut h = harness(ThemeKind::Dark);
        let original = h.state().session.document().clone();
        controls(&mut h, "Blue", "Rename tag…");
        h.event(egui::Event::Text(" draft".into()));
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().tag_definition.is_none());
        assert_eq!(h.state().session.document(), &original);
        controls(&mut h, "Blue", "Rename tag…");
        let c = h.state().canvas;
        h.state_mut().session.dispatch("tag.put", &json!({"canvas":c,"tag":{"id":TAG,"title":"Changed elsewhere","color":"pink"}})).unwrap();
        h.run();
        assert!(h.state().tag_definition.is_none());
        let tag = h.state().session.document().canvases().get(&c).unwrap().tags().get(&TAG).unwrap();
        assert_eq!(tag.title, "Changed elsewhere");
        assert_eq!(tag.color.as_deref(), Some("pink"));
    }

    #[test]
    fn unicode_name_byte_limit_and_equal_document_session_replacement_preserve_the_document() {
        let mut h = harness(ThemeKind::Light);
        let original = h.state().session.document().clone();
        controls(&mut h, "Blue", "Rename tag…");
        h.event(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
        h.event(egui::Event::Text("界".repeat(1400)));
        h.run();
        h.get_by_label("Use a name of at most 4096 UTF-8 bytes.");
        let pos = h.get_by_role_and_label(Role::Button, "Save tag name").rect().center();
        h.hover_at(pos);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.run();
        }
        assert_eq!(h.state().session.document(), &original);
        let c = h.state().canvas;
        h.state_mut().replace_session(Session::new(original.clone()).unwrap(), c).unwrap();
        h.run();
        assert!(h.state().tag_definition.is_none());
        assert_eq!(h.state().session.document(), &original);
    }
    #[test]
    #[ignore = "manual public-safe PR evidence"]
    fn render_tag_definition_evidence() {
        let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
        std::fs::create_dir_all(&folder).unwrap();
        let mut h = harness_with_render(ThemeKind::Dark, true);
        let before = h.state().session.document().clone();
        keyboard_controls(&mut h, "Blue");
        h.render().unwrap().save(folder.join("keyboard-menu.png")).unwrap();
        h.key_press(egui::Key::Enter);
        h.run();
        h.render().unwrap().save(folder.join("rename.png")).unwrap();
        h.get_by_role_and_label(Role::Button, "Cancel tag edit").click();
        h.run();
        controls(&mut h, "Blue", "Delete tag…");
        h.render().unwrap().save(folder.join("delete.png")).unwrap();
        assert_eq!(h.state().session.document(), &before, "opening definition controls never edits the document");
    }
}
