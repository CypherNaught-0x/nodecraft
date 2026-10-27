//! Public UI evidence uses each source revision's actual complete builtin catalog.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

struct Scene {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}

fn scene(builder: egui_kittest::HarnessBuilder<Scene>, theme: crate::theme::ThemeKind) -> Harness<'static, Scene> {
    let catalog = Catalog::builtin().unwrap();
    let wiring = Wiring::new(Modality::Image).with(Modality::Image, 1).with_inline_prompt();
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("Magnific").unwrap();
    let mut harness = builder.with_size(egui::vec2(1100., 750.)).build_ui_state(
        move |ui, scene: &mut Scene| {
            if scene.outcome == Outcome::Open {
                scene.outcome = scene.picker.show(ui.ctx(), &scene.catalog, theme.tokens());
            }
        },
        Scene { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&harness.ctx, theme);
    harness.run();
    harness
}

#[test]
#[ignore = "native GPU evidence; NODECRAFT_MAGNIFIC38_RENDER_ROOT and NODECRAFT_MAGNIFIC38_STAGE"]
fn magnific38_native_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MAGNIFIC38_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_MAGNIFIC38_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = scene(Harness::builder().wgpu(), theme);
        harness.hover_at(egui::pos2(1090., 735.));
        harness.run();
        assert_eq!(harness.query_by_label("Magnific Precision V2").is_some(), stage == "after");
        harness.render().unwrap().save(root.join(format!("{theme:?}-picker-{stage}.png"))).unwrap();
    }
}
