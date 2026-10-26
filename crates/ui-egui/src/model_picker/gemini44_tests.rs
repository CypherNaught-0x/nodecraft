use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}

#[test]
fn gemini44_model_is_selectable_with_an_unknown_usage_price() {
    let catalog = Catalog::builtin().unwrap();
    let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Text).with_inline_prompt(), |_| Ok(())).unwrap();
    picker.set_query("Gemini 3.8").unwrap();
    let state = View { catalog, picker, outcome: Outcome::Open };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 650.0)).build_ui_state(
        |ui, state: &mut View| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
            }
        },
        state,
    );
    h.run();
    assert_eq!(price_label(h.state().catalog.get("gemini-llm.3.8-flash").unwrap()), "Usage based (USD)");
    h.get_by_label("Gemini 3.8 Flash").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::Chosen("gemini-llm.3.8-flash".into()));
}

#[test]
#[ignore = "manual public-safe picker capture; GEMINI44_RENDER_DIR"]
fn gemini44_picker_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("GEMINI44_RENDER_DIR").unwrap());
    for before in [true, false] {
        let builtin = Catalog::builtin().unwrap();
        // The only picker change is this catalog row. The before uses the exact original
        // catalog (all rows except Gemini 3.8), rendered by the unchanged shipping picker.
        let catalog = Catalog::from_models(builtin.models().filter(|model| !before || model.provider != "gemini-llm").cloned()).unwrap();
        let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Text).with_inline_prompt(), |_| Ok(())).unwrap();
        picker.set_query("Gemini 3.8").unwrap();
        let state = View { catalog, picker, outcome: Outcome::Open };
        let mut h = Harness::builder()
            .wgpu()
            .with_size(egui::vec2(1000.0, 650.0))
            .build_ui_state(|ui, state: &mut View| state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK), state);
        crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
        h.run();
        h.render().unwrap().save(dir.join(if before { "gemini44-before.png" } else { "gemini44-after.png" })).unwrap();
    }
}
