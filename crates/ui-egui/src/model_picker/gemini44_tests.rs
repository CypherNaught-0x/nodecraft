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

fn video84_view() -> View {
    let catalog = Catalog::builtin().unwrap();
    let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Text).with_inline_prompt().with(Modality::Video, 1), |_| Ok(())).unwrap();
    picker.set_query("Gemini 3.8").unwrap();
    picker.refilter(&catalog);
    View { catalog, picker, outcome: Outcome::Open }
}

/// #84: a text card with a video wired offers Gemini 3.8 Flash, and its details say how it
/// reads the video.
#[test]
fn gemini84_a_wired_video_offers_gemini_and_its_details_say_how_it_reads() {
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 650.0)).build_ui_state(
        |ui, state: &mut View| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
            }
        },
        video84_view(),
    );
    h.run();
    h.get_by_label("Gemini 3.8 Flash").hover();
    for _ in 0..90 {
        h.step();
    }
    h.get_by_label("video-to-text to text");
    h.get_by_label("Reads up to 1048576 tokens; answers up to 65536 tokens; video: a frame every second, at most 10800 frames; videos up to 14.9 MB together");
    h.get_by_label("Gemini 3.8 Flash").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::Chosen("gemini-llm.3.8-flash".into()));
}

#[test]
#[ignore = "manual public-safe picker capture; GEMINI84_RENDER_DIR"]
fn gemini84_picker_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("GEMINI84_RENDER_DIR").unwrap());
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::vec2(1000.0, 650.0))
        .build_ui_state(|ui, state: &mut View| state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK), video84_view());
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h.get_by_label("Gemini 3.8 Flash").hover();
    for _ in 0..90 {
        h.step();
    }
    h.render().unwrap().save(dir.join("gemini84-video-picker.png")).unwrap();
}
