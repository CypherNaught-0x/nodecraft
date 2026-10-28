//! Real builtin picker evidence, without catalog filtering or vendor media.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct Scene {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<Scene>, theme: crate::theme::ThemeKind, lipsync: bool) -> Harness<'static, Scene> {
    let catalog = Catalog::builtin().unwrap();
    let wiring = Wiring::new(Modality::Video).with(if lipsync { Modality::Video } else { Modality::Image }, 1).with(Modality::Audio, 1);
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("HeyGen").unwrap();
    let mut h = builder.with_size(egui::vec2(1100., 800.)).build_ui_state(
        move |ui, state: &mut Scene| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, theme.tokens());
            }
        },
        Scene { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}
#[test]
#[ignore = "native GPU evidence; NODECRAFT_HEYGEN43_RENDER_ROOT and NODECRAFT_HEYGEN43_STAGE"]
fn heygen43_native_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_HEYGEN43_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_HEYGEN43_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder().wgpu(), theme, false);
        h.hover_at(egui::pos2(1090., 785.));
        h.run();
        assert_eq!(h.query_by_label("HeyGen Photo Avatar 4 (fal)").is_some(), stage == "after");
        h.render().unwrap().save(root.join(format!("{theme:?}-picker-{stage}.png"))).unwrap();
    }
}
