//! Model comparison remains usable without hovering, including a narrow window.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
    theme: crate::theme::ThemeKind,
}
fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, width: f32) -> Harness<'static, View> {
    let catalog = Catalog::builtin().unwrap();
    let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Image).with_inline_prompt(), |_| Ok(())).unwrap();
    picker.set_query("GPT Image 2.5").unwrap();
    let view = View { catalog, picker, outcome: Outcome::Open, theme };
    let mut h = builder.with_size(egui::vec2(width, 750.0)).build_ui_state(
        |ui, view: &mut View| {
            if view.outcome == Outcome::Open {
                let t = match view.theme {
                    crate::theme::ThemeKind::Dark => &crate::theme::DARK,
                    crate::theme::ThemeKind::Light => &crate::theme::LIGHT,
                };
                view.outcome = view.picker.show(ui.ctx(), &view.catalog, t);
            }
        },
        view,
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}
#[test]
fn catalog963_descriptions_and_selection_tiers_are_visible_in_both_themes_and_narrow_windows() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for width in [1100.0, 320.0] {
            let mut h = scene(Harness::builder(), theme, width);
            assert_eq!(h.query_all_by_label_contains("Selection tier 4/5").count(), 2);
            assert!(h.query_all_by_label_contains("editorial priority").next().is_some());
            for id in ["openai-images.flare", "openai-images.sunburst"] {
                let model = h.state().catalog.get(id).unwrap();
                let description = h.get_by_label(&model.description);
                assert!(description.rect().right() <= width, "description extends outside the window");
                assert!(h.get_by_label(&model.label).rect().right() <= width, "choice extends outside the window");
            }
            h.get_by_label("GPT Image 2.5 Flare").click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen("openai-images.flare".into()));
        }
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_CATALOG963_RENDER_ROOT and NODECRAFT_CATALOG963_RENDER_STAGE"]
fn catalog963_render_model_comparison() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_CATALOG963_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_CATALOG963_RENDER_STAGE").unwrap();
    assert!(matches!(stage.as_str(), "before" | "after"));
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (name, width) in [("wide", 1100.0), ("narrow", 320.0)] {
            let mut h = scene(Harness::builder().wgpu(), theme, width);
            h.hover_at(egui::pos2(width - 5.0, 700.0));
            h.run();
            h.render().unwrap().save(root.join(format!("{stage}-{theme:?}-{name}.png"))).unwrap();
        }
    }
}
