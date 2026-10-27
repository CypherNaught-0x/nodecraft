//! Alibaba's four catalog entries and rates through the real picker, in both themes.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const MODELS: [&str; 4] = ["fal.qwen-image3", "fal.qwen-image3-edit", "fal.wan3", "fal.wan3-i2v"];
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}

fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, after: bool, wiring: Wiring) -> Harness<'static, View> {
    let builtin = Catalog::builtin().unwrap();
    // Keep an existing fal entry in both images and all four additions together after the change.
    let catalog =
        Catalog::from_models(builtin.models().filter(|model| model.id == "fal.flux-schnell" || after && MODELS.contains(&model.id.as_str())).cloned()).unwrap();
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("").unwrap();
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
fn alibaba32_picker_selects_all_four_models_and_shows_image_and_second_rates_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (id, label, output, images, rate) in [
            ("fal.qwen-image3", "Qwen-Image 3 (fal)", Modality::Image, 0, "From 0.04 USD / image"),
            ("fal.qwen-image3-edit", "Qwen-Image 3 edit (fal)", Modality::Image, 3, "From 0.04 USD / image"),
            ("fal.wan3", "Wan 3.0 (fal)", Modality::Video, 0, "From 0.05 USD / second"),
            ("fal.wan3-i2v", "Wan 3.0 frames (fal)", Modality::Video, 2, "From 0.05 USD / second"),
        ] {
            let mut wiring = Wiring::new(output).with_inline_prompt();
            if images > 0 {
                wiring = wiring.with(Modality::Image, images);
            }
            let mut h = scene(Harness::builder(), theme, true, wiring);
            assert_eq!(price_label(h.state().catalog.get(id).unwrap()), rate);
            assert!(h.query_all_by_label_contains(rate).next().is_some(), "the rate must appear in the rendered picker");
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}

#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_ALIBABA32_RENDER_ROOT"]
fn alibaba32_render_model_picker() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_ALIBABA32_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for after in [false, true] {
            // No output restriction: both families appear; reference modes honestly show their missing-input reason.
            let mut h = scene(Harness::builder().wgpu(), theme, after, Wiring::default().with_inline_prompt());
            h.hover_at(egui::pos2(990.0, 680.0));
            h.run();
            for label in ["Qwen-Image 3 (fal)", "Wan 3.0 (fal)"] {
                assert_eq!(h.query_all_by_label_contains(label).next().is_some(), after);
            }
            h.render().unwrap().save(root.join(format!("{theme:?}-{}.png", if after { "after" } else { "before" }))).unwrap();
        }
    }
}
