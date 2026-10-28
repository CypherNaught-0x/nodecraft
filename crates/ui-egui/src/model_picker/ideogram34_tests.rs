use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
    theme: crate::theme::ThemeKind,
}
fn view(theme: crate::theme::ThemeKind, before: bool) -> View {
    let builtin = Catalog::builtin().unwrap();
    let catalog = Catalog::from_models(builtin.models().filter(|model| !before || model.id != "fal.ideogram-v3").cloned()).unwrap();
    let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Image).with_inline_prompt(), |_| Ok(())).unwrap();
    picker.set_query("Ideogram").unwrap();
    View { catalog, picker, outcome: Outcome::Open, theme }
}
fn show(ui: &mut egui::Ui, view: &mut View) {
    if view.outcome == Outcome::Open {
        let tokens = match view.theme {
            crate::theme::ThemeKind::Dark => &crate::theme::DARK,
            crate::theme::ThemeKind::Light => &crate::theme::LIGHT,
        };
        view.outcome = view.picker.show(ui.ctx(), &view.catalog, tokens);
    }
}
#[test]
fn ideogram34_real_picker_shows_the_fixed_price_and_selects_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = Harness::builder().with_size(egui::vec2(1100.0, 750.0)).build_ui_state(show, view(theme, false));
        crate::theme::apply(&h.ctx, theme);
        h.run();
        assert_eq!(price_label(h.state().catalog.get("fal.ideogram-v3").unwrap()), "0.06 USD / call");
        assert!(h.query_all_by_label_contains("0.06 USD / call").next().is_some());
        h.get_by_label("Ideogram V3 Balanced (fal)").click();
        h.run();
        assert_eq!(h.state().outcome, Outcome::Chosen("fal.ideogram-v3".into()));
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_IDEOGRAM34_RENDER_ROOT"]
fn ideogram34_render_picker_before_and_after() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_IDEOGRAM34_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for before in [true, false] {
            let mut h = Harness::builder().wgpu().with_size(egui::vec2(1100.0, 750.0)).build_ui_state(show, view(theme, before));
            crate::theme::apply(&h.ctx, theme);
            h.run();
            assert_eq!(h.query_by_label("Ideogram V3 Balanced (fal)").is_some(), !before);
            h.render().unwrap().save(root.join(format!("{theme:?}-{}.png", if before { "before" } else { "after" }))).unwrap();
        }
    }
}
