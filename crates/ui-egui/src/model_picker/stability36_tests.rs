//! Three first-party Stability families, selectable with their real roles and dated rates.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn scene(builder: egui_kittest::HarnessBuilder<View>, theme: crate::theme::ThemeKind, after: bool, wiring: Wiring) -> Harness<'static, View> {
    let builtin = Catalog::builtin().unwrap();
    let catalog =
        Catalog::from_models(builtin.models().filter(|model| model.id == "fal.flux-schnell" || after && model.provider == "stability").cloned()).unwrap();
    let picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
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
        View { catalog, picker, outcome: Outcome::Open },
    );
    crate::theme::apply(&h.ctx, theme);
    h.run();
    h
}
#[test]
fn stability36_picker_selects_image_audio_and_mesh_with_per_call_rates_in_both_themes() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for (id, label, output, rate) in [
            ("stability.image-core", "Stable Image Core", Modality::Image, "0.03 USD / call"),
            ("stability.audio3", "Stable Audio 3 Large", Modality::Audio, "0.08 USD / call"),
            ("stability.fast-3d", "Stable Fast 3D", Modality::Mesh, "0.1 USD / call"),
        ] {
            let wiring = if output == Modality::Mesh { Wiring::new(output).with(Modality::Image, 1) } else { Wiring::new(output).with_inline_prompt() };
            let mut h = scene(Harness::builder(), theme, true, wiring);
            assert_eq!(price_label(h.state().catalog.get(id).unwrap()), rate);
            assert!(h.query_all_by_label_contains(rate).next().is_some());
            h.get_by_label(label).click();
            h.run();
            assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
        }
    }
}
#[test]
#[ignore = "manual native GPU evidence; NODECRAFT_STABILITY36_RENDER_ROOT"]
fn stability36_render_model_picker() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_STABILITY36_RENDER_ROOT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for after in [false, true] {
            let mut h = scene(Harness::builder().wgpu(), theme, after, Wiring::default().with_inline_prompt());
            h.state_mut().picker.set_query("stability").unwrap();
            h.hover_at(egui::pos2(990.0, 680.0));
            h.run();
            for label in ["Stable Image Core", "Stable Audio 3 Large", "Stable Fast 3D"] {
                assert_eq!(h.query_all_by_label_contains(label).next().is_some(), after, "{label}");
            }
            h.render().unwrap().save(root.join(format!("{theme:?}-{}.png", if after { "after" } else { "before" }))).unwrap();
        }
    }
}
