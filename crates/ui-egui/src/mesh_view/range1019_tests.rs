use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const RANGE_REASON: &str = "3D preview unavailable at this coordinate range";
fn square(side: f32, offset: f32) -> nodecraft_media::Mesh {
    nodecraft_media::Mesh {
        positions: vec![[offset, offset, 0.], [offset + side, offset, 0.], [offset + side, offset + side, 0.], [offset, offset + side, 0.]],
        normals: vec![[0., 0., 1.]; 4],
        colors: vec![[1., 0.25, 0.5, 1.]; 4],
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}
#[test]
fn usdz1019_extreme_interchange_is_retained_but_actual_orbit_preview_is_unavailable() {
    let mesh = square(1e30, 1e30);
    let bytes = nodecraft_media::encode_usdz(&mesh, 1 << 20).unwrap();
    let decoded = decode_mesh(&bytes, &Default::default()).unwrap();
    assert_eq!(decoded, mesh, "valid interchange and original coordinates are retained");
    let (center, radius) = decoded.bounds().unwrap().checked_sphere().unwrap();
    let actual = Orbit::default().view_projection(center, radius, 1.);
    assert!(actual.iter().any(|v| !v.is_finite()), "exercise the actual pre-existing f32 camera limitation");
    assert_eq!(Model::decode(&bytes).err(), Some(RANGE_REASON), "a bounded actionable outcome must precede GPU upload");
    let mut assets = nodecraft_format::AssetStore::default();
    let hash = assets.insert(&bytes).unwrap();
    let asset = AssetRef { hash, mime: "model/vnd.usdz+zip".parse().unwrap(), meta: Default::default() };
    let mut previews = Meshes::default();
    previews.inline(hash, &bytes);
    assert!(matches!(
        previews.request(&egui::Context::default(), &asset, || panic!("cached range refusal never rereads")),
        MeshPreview::Unavailable(RANGE_REASON)
    ));
    assert_eq!(previews.bytes(), 0, "unusable models are not held in the decoded preview cache");
    assert_eq!(assets.get(&hash).unwrap(), bytes, "preview refusal never mutates the original asset");
    let glb = nodecraft_media::encode_glb(&decoded).unwrap();
    assert_eq!(decode_mesh(&glb, &Default::default()).unwrap(), mesh, "conversion remains available without rescaling");
}

#[test]
fn usdz1019_current_orbit_range_refusal_is_visible_and_reset_recovers_in_both_themes() {
    let mesh = square(4e18, 0.);
    let bytes = nodecraft_media::encode_usdz(&mesh, 1 << 20).unwrap();
    let model = Arc::new(Model::decode(&bytes).unwrap());
    assert!(Orbit::default().checked_view_projection(model.center, model.radius, 1.).is_some());
    let orbit = Orbit { zoom: Orbit::MIN_ZOOM, ..Default::default() };
    assert!(orbit.view_projection(model.center, model.radius, 1.).iter().any(|v| !v.is_finite()));
    assert!(orbit.checked_view_projection(model.center, model.radius, 1.).is_none());
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut h = Harness::builder().with_size(Vec2::new(600., 400.)).build_ui_state(
            move |ui, state: &mut (Arc<Model>, MeshView)| {
                let area = Rect::from_min_size(ui.min_rect().min, Vec2::new(560., 340.));
                show(
                    ui,
                    area,
                    1.,
                    CardMesh {
                        preview: MeshPreview::Ready { hash: Blake3::from_bytes([101; 32]), model: Arc::clone(&state.0) },
                        view: &mut state.1,
                        gpu: true,
                    },
                    theme.tokens(),
                );
            },
            (Arc::clone(&model), MeshView { orbit, ..Default::default() }),
        );
        crate::theme::apply(&h.ctx, theme);
        h.run();
        assert!(h.query_by_label(RANGE_REASON).is_some());
        assert!(h.query_by_label("Photo").is_none(), "an unavailable view cannot offer captures");
        h.get_by_label("Reset").click();
        h.run_steps(3);
        assert!(h.query_by_label("3D model, 2 triangles, shaded view").is_some());
        assert!(h.query_by_label("Photo").is_some());
        assert!(h.state().1.orbit.checked_view_projection(model.center, model.radius, 1.).is_some());
    }
    assert_eq!(decode_mesh(&bytes, &Default::default()).unwrap(), mesh, "presentation changes never rescale valid geometry");
}
