use super::*;
use egui_kittest::{Harness, kittest::Queryable};

#[test]
fn recraft35_picker_distinguishes_raster_and_svg_and_selects_each_at_its_price() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (id, label, micros) in [("fal.recraft41", "Recraft V4.1 raster (fal)", 35_000), ("fal.recraft41-vector", "Recraft V4.1 SVG (fal)", 80_000)] {
            let builtin = Catalog::builtin().unwrap();
            let catalog = Catalog::from_models(builtin.models().filter(|m| m.id.starts_with("fal.recraft41")).cloned()).unwrap();
            assert_eq!(catalog.get(id).unwrap().quote(&Default::default()).unwrap().unwrap().micros, micros);
            let wiring = Wiring::new(Modality::Image).with_inline_prompt();
            let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
            picker.set_query("").unwrap();
            let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
                move |ui, state: &mut (ModelPicker, Outcome)| {
                    if state.1 == Outcome::Open {
                        state.1 = state.0.show(ui.ctx(), &catalog, theme.tokens());
                    }
                },
                (picker, Outcome::Open),
            );
            crate::theme::apply(&h.ctx, theme);
            h.run();
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().1, Outcome::Chosen(id.into()));
        }
    }
}
