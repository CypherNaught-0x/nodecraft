//! Rodin's four model rows, per-call rates and actual picker selection in both themes.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const MODELS: [&str; 4] = ["fal.rodin25", "fal.rodin25-images", "fal.rodin25-fast", "fal.rodin25-fast-images"];
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, after: bool, wiring: Wiring) -> Harness<'static, View> {
    let builtin = Catalog::builtin().unwrap();
    // The existing Trellis row provides a genuine catalog baseline; the after adds all four Rodin rows.
    let catalog =
        Catalog::from_models(builtin.models().filter(|model| model.id == "fal.trellis2" || after && MODELS.contains(&model.id.as_str())).cloned()).unwrap();
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("").unwrap();
    let state = View { catalog, picker, outcome: Outcome::Open };
    let mut h = builder.with_size(egui::vec2(1100.0, 800.0)).build_ui_state(
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
fn rodin41_picker_selects_all_four_models_with_appropriate_image_counts_and_exact_call_rates() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (id, label, rate, images) in [
            ("fal.rodin25", "Rodin 2.5 text (fal)", "0.4 USD / call", 0),
            ("fal.rodin25-images", "Rodin 2.5 images (fal)", "0.4 USD / call", 5),
            ("fal.rodin25-fast", "Rodin 2.5 fast text (fal)", "0.1 USD / call", 0),
            ("fal.rodin25-fast-images", "Rodin 2.5 fast images (fal)", "0.1 USD / call", 5),
        ] {
            let mut wiring = Wiring::new(Modality::Mesh).with_inline_prompt();
            if images > 0 {
                wiring = wiring.with(Modality::Image, images);
            }
            let mut h = scene(Harness::builder(), theme, true, wiring);
            assert_eq!(price_label(h.state().catalog.get(id).unwrap()), rate);
            assert!(h.query_all_by_label_contains(rate).next().is_some(), "rate appears in the rendered picker");
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_RODIN41_RENDER_ROOT"]
fn rodin41_render_model_picker() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_RODIN41_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for after in [false, true] {
            let mut h = scene(Harness::builder().wgpu(), theme, after, Wiring::new(Modality::Mesh).with_inline_prompt());
            h.hover_at(egui::pos2(1090.0, 780.0));
            h.run();
            if !after {
                h.get_by_label("Trellis 2 (fal)");
            }
            // The picker virtualizes its list. Capture the two compatible text rows;
            // image rows are selected with their real wiring in the regular test above.
            for label in ["Rodin 2.5 text (fal)", "Rodin 2.5 fast text (fal)"] {
                assert_eq!(h.query_all_by_label_contains(label).next().is_some(), after);
            }
            for rate in ["0.4 USD / call", "0.1 USD / call"] {
                assert_eq!(h.query_all_by_label_contains(rate).next().is_some(), after);
            }
            h.render().unwrap().save(root.join(format!("{theme:?}-{}.png", if after { "after" } else { "before" }))).unwrap();
        }
    }
}
