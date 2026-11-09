use super::*;
use nodecraft_types::{Currency, Modality, Money, Value};

#[test]
fn comfy3d24_local_models_require_one_image_and_quote_free_meshes() {
    let catalog = Catalog::builtin().unwrap();
    for id in ["comfyui.hunyuan3d-2", "comfyui.hunyuan3d-2.1"] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "comfyui");
        assert_eq!(model.quote(&Default::default()).unwrap(), Some(Money::zero(Currency::USD)));
        assert!(Wiring::new(Modality::Mesh).with(Modality::Image, 1).with_inline_prompt().fit(model).is_ok());
        for wiring in [
            Wiring::new(Modality::Mesh),
            Wiring::new(Modality::Image).with(Modality::Image, 1),
            Wiring::new(Modality::Mesh).with(Modality::Image, 2),
            Wiring::new(Modality::Mesh).with(Modality::Image, 1).with(Modality::Text, 1),
        ] {
            assert!(wiring.fit(model).is_err(), "{id}: {wiring:?}");
        }
        let contract = model.parameter_contract.as_ref().unwrap();
        assert_eq!(contract.namespace, id);
        let checkpoint = contract.rules.iter().find(|r| r.param == "checkpoint").unwrap();
        assert_eq!(checkpoint.options.as_ref().unwrap().id, "comfyui.image_checkpoints");
        assert!(model.params.resolve(&[("cfg".into(), Value::Text("bad".into()))].into(), false).is_err());
    }
}
