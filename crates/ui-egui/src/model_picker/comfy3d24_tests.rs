//! Native before/after evidence uses the real catalog, without filtering out models.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

struct Scene {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}

fn scene(builder: egui_kittest::HarnessBuilder<Scene>, theme: crate::theme::ThemeKind) -> Harness<'static, Scene> {
    let catalog = Catalog::builtin().unwrap();
    let wiring = Wiring::new(Modality::Mesh).with(Modality::Image, 1).with_inline_prompt();
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("Hunyuan3D 2.").unwrap();
    let mut h = builder.with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
        move |ui, state: &mut Scene| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, theme.tokens());
            }
        },
        Scene { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}

#[test]
#[ignore = "native GPU evidence; NODECRAFT_COMFY3D24_RENDER_ROOT and NODECRAFT_COMFY3D24_STAGE"]
fn comfy3d24_model_picker_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_COMFY3D24_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_COMFY3D24_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = scene(Harness::builder().wgpu(), theme);
        h.hover_at(egui::pos2(990.0, 680.0));
        h.run();
        h.render().unwrap().save(root.join(format!("{theme:?}-{stage}.png"))).unwrap();
    }
}

#[test]
fn comfy3d24_picker_selects_each_local_image_to_mesh_model() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (id, label) in [("comfyui.hunyuan3d-2", "Hunyuan3D 2.0 (local)"), ("comfyui.hunyuan3d-2.1", "Hunyuan3D 2.1 (local)")] {
            let mut h = scene(Harness::builder(), theme);
            assert_eq!(price_label(h.state().catalog.get(id).unwrap()), "Free");
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
