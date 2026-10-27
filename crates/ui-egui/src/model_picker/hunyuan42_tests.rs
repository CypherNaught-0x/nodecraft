//! Hunyuan3D catalog models and per-call mesh rates in the real picker.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, after: bool, images: usize) -> Harness<'static, View> {
    let builtin = Catalog::builtin().unwrap();
    let catalog = Catalog::from_models(builtin.models().filter(|m| after || !m.id.starts_with("fal.hunyuan3d-v3")).cloned()).unwrap();
    let mut wiring = Wiring::new(Modality::Mesh).with_inline_prompt();
    if images > 0 {
        wiring = wiring.with(Modality::Image, images);
    }
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("Hunyuan").unwrap();
    let state = View { catalog, picker, outcome: Outcome::Open };
    let mut h = builder.with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
        move |ui, state: &mut View| {
            if state.outcome == Outcome::Open {
                let tokens = match theme {
                    crate::theme::ThemeKind::Dark => &crate::theme::DARK,
                    crate::theme::ThemeKind::Light => &crate::theme::LIGHT,
                };
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, tokens);
            }
        },
        state,
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}
#[test]
fn hunyuan42_picker_selects_text_photo_and_sketch_and_labels_rates_per_call() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (images, id, label) in [
            (0, "fal.hunyuan3d-v3", "Hunyuan3D V3 text (fal)"),
            (1, "fal.hunyuan3d-v3-image", "Hunyuan3D V3 photo (fal)"),
            (1, "fal.hunyuan3d-v3-sketch", "Hunyuan3D V3 sketch (fal)"),
        ] {
            let mut h = scene(Harness::builder(), theme, true, images);
            assert!(price_label(h.state().catalog.get(id).unwrap()).ends_with(" / call"));
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_HUNYUAN42_RENDER_ROOT"]
fn hunyuan42_render_model_picker() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_HUNYUAN42_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for after in [false, true] {
            let mut h = scene(Harness::builder().wgpu(), theme, after, 0);
            h.hover_at(egui::pos2(990.0, 680.0));
            h.run();
            h.render().unwrap().save(root.join(format!("{theme:?}-{}.png", if after { "after" } else { "before" }))).unwrap();
        }
    }
}
