//! LTX catalog rows are discoverable, typed and honest about unknown cost.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, after: bool, images: usize) -> Harness<'static, View> {
    let builtin = Catalog::builtin().unwrap();
    let catalog = Catalog::from_models(builtin.models().filter(|m| after || !m.id.starts_with("fal.ltx23")).cloned()).unwrap();
    let mut wiring = Wiring::new(Modality::Video).with_inline_prompt();
    if images > 0 {
        wiring = wiring.with(Modality::Image, images);
    }
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("LTX").unwrap();
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
fn ltx30_model_picker_selects_the_mode_fitting_the_wired_frames_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for images in [0, 1, 2] {
            let mut h = scene(Harness::builder(), theme, true, images);
            assert_eq!(price_label(h.state().catalog.get("fal.ltx23").unwrap()), "Usage based (USD)");
            let (id, label) = if images == 0 { ("fal.ltx23", "LTX 2.3 Pro (fal)") } else { ("fal.ltx23-i2v", "LTX 2.3 Pro frames (fal)") };
            assert!(h.query_all_by_label_contains("time depends on the clip").next().is_some());
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_LTX30_RENDER_ROOT"]
fn ltx30_render_model_picker() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_LTX30_RENDER_ROOT").unwrap());
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
