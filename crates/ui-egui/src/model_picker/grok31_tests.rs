//! Grok image/video rows and reference fees in the real picker.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, after: bool, images: usize, output: Modality) -> Harness<'static, View> {
    let builtin = Catalog::builtin().unwrap();
    let catalog = Catalog::from_models(builtin.models().filter(|m| after || !m.id.starts_with("fal.grok-")).cloned()).unwrap();
    let mut wiring = Wiring::new(output).with_inline_prompt();
    if images > 0 {
        wiring = wiring.with(Modality::Image, images);
    }
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("Grok").unwrap();
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
fn grok31_picker_selects_the_matching_image_and_video_mode_and_shows_input_fees() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (output, images, id, label) in [
            (Modality::Image, 0, "fal.grok-image2", "Grok Imagine Image 2.0 (fal)"),
            (Modality::Image, 5, "fal.grok-image2-edit", "Grok Imagine Image 2.0 edit (fal)"),
            (Modality::Video, 0, "fal.grok-video15", "Grok Imagine Video 1.5 (fal)"),
            (Modality::Video, 1, "fal.grok-video15-i2v", "Grok Imagine Video 1.5 frame (fal)"),
        ] {
            let mut h = scene(Harness::builder(), theme, true, images, output);
            if images > 0 {
                assert!(price_label(h.state().catalog.get(id).unwrap()).contains("+ 0.01 USD / input image"));
            }
            assert!(h.query_all_by_label_contains("Moderation failures may be billed").next().is_some());
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_GROK31_RENDER_ROOT"]
fn grok31_render_model_picker() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_GROK31_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (output, name) in [(Modality::Image, "image"), (Modality::Video, "video")] {
            for after in [false, true] {
                let mut h = scene(Harness::builder().wgpu(), theme, after, 0, output);
                h.hover_at(egui::pos2(990.0, 680.0));
                h.run();
                h.render().unwrap().save(root.join(format!("{theme:?}-{name}-{}.png", if after { "after" } else { "before" }))).unwrap();
            }
        }
    }
}
