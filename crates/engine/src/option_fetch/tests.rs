use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn values_keep_the_server_order_and_skip_what_cant_be_offered() {
    let long = "x".repeat(MAX_VALUE_BYTES + 1);
    let listed = values(&strings(&["b", "", "a", "b", &long])).unwrap();
    assert_eq!(listed, Values { values: strings(&["b", "a"]), skipped: 3 });
    let many: Vec<String> = (0..=MAX_OPTIONS).map(|i| i.to_string()).collect();
    assert!(values(&many).is_err(), "refused whole, not truncated");
    assert!(values(&many[..MAX_OPTIONS]).is_ok());
}

#[test]
fn only_unscoped_known_sources_are_fetched() {
    let want = |source: &str, scope: Option<&str>| Wanted { owner: "m".into(), source: source.into(), scope: scope.map(str::to_owned) };
    let wanted = [
        want("comfyui.checkpoints", None),
        want("comfyui.checkpoints", None),
        want("comfyui.loras", Some("checkpoint")),
        want("elevenlabs.voices", None),
        want("comfyui.samplers", None),
    ];
    assert_eq!(fetchable(&wanted), strings(&["comfyui.checkpoints", "comfyui.samplers"]));
}

#[test]
fn every_source_id_is_a_valid_list_id() {
    for (id, class, input) in COMFY_SOURCES {
        assert!(nodecraft_types::OptionLists::default().set(OptionKey { owner: "m", source: id, scope: None }, Vec::new()).is_ok(), "{id}");
        assert!(!class.is_empty() && !input.is_empty());
    }
}

#[test]
fn the_builtin_catalog_asks_for_the_sdxl_checkpoints() {
    let catalog = GenerationCatalog::builtin().unwrap();
    let wanted = wanted(&catalog);
    assert!(wanted.contains(&Wanted { owner: "comfyui.sdxl".into(), source: "comfyui.checkpoints".into(), scope: None }), "{wanted:?}");
}
