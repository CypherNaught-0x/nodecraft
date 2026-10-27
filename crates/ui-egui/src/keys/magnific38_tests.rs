//! Key-panel evidence never reads a real keychain or sends a network request.
use super::*;

fn themed(builder: egui_kittest::HarnessBuilder<State>, theme: crate::theme::ThemeKind) -> Harness<'static, State> {
    let state = sync_state(Arc::new(MemoryStore::default()), &[]);
    let mut harness = builder.with_size(egui::vec2(1250., 760.)).build_ui_state(
        move |ui, state: &mut State| {
            let changes = state.panel.show(ui.ctx(), &mut state.session, &state.settings, theme.tokens());
            state.changes.extend(changes);
        },
        state,
    );
    crate::theme::apply(&harness.ctx, theme);
    harness.run();
    harness
}

#[test]
#[ignore = "native GPU evidence; NODECRAFT_MAGNIFIC38_RENDER_ROOT and NODECRAFT_MAGNIFIC38_STAGE"]
fn magnific38_native_keys_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MAGNIFIC38_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_MAGNIFIC38_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = themed(Harness::builder().wgpu(), theme);
        harness.hover_at(egui::pos2(1240., 745.));
        harness.run();
        assert_eq!(harness.query_by_label("magnific").is_some(), stage == "after");
        assert!(harness.state().panel.key.is_empty());
        assert!(harness.state().changes.is_empty());
        harness.render().unwrap().save(root.join(format!("{theme:?}-keys-{stage}.png"))).unwrap();
    }
}
