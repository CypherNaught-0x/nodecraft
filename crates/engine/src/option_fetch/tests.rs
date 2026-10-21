use super::*;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// A ComfyUI-style list: each value is its own label.
fn plain(values: &[String]) -> impl Iterator<Item = (&str, &str)> {
    values.iter().map(|value| (value.as_str(), value.as_str()))
}

fn choices(values: &[(&str, &str)]) -> Vec<Choice> {
    values.iter().map(|(value, label)| Choice { value: (*value).to_owned(), label: (*label).to_owned() }).collect()
}

#[test]
fn values_keep_the_server_order_and_skip_what_cant_be_offered() {
    let long = "x".repeat(MAX_VALUE_BYTES + 1);
    let listed = values(plain(&strings(&["b", "", "a", "b", &long, " \t"])), 0).unwrap();
    assert_eq!(listed, Values { values: choices(&[("b", "b"), ("a", "a")]), skipped: 4 });
    let many: Vec<String> = (0..=MAX_OPTIONS).map(|i| i.to_string()).collect();
    assert!(values(plain(&many), 0).is_err(), "refused whole, not truncated");
    assert!(values(plain(&many[..MAX_OPTIONS]), 0).is_ok());
    // Under the count but past the store's bytes: refused with its own reason, not the store's.
    let wide: Vec<String> = (0..2000).map(|i| format!("{i:0>width$}", width = MAX_VALUE_BYTES)).collect();
    assert_eq!(values(plain(&wide), 0), Err("the server's list is larger than 16 MiB".into()));
}

#[test]
fn labelled_values_keep_their_labels_and_add_to_what_the_provider_skipped() {
    let listed = values([("id-1", "Anime"), ("id-2", " "), ("id-1", "Again"), ("id-3", "Ink")], 2).unwrap();
    assert_eq!(listed, Values { values: choices(&[("id-1", "Anime"), ("id-3", "Ink")]), skipped: 4 });
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
        want("higgsfield.restyle_presets", None),
        want("higgsfield.soul_ids", None),
    ];
    assert_eq!(fetchable(&wanted), strings(&["comfyui.checkpoints", "comfyui.samplers", "higgsfield.restyle_presets", "higgsfield.soul_ids"]));
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
    let restyle = Wanted { owner: "higgsfield.genjutsu-restyle".into(), source: "higgsfield.restyle_presets".into(), scope: None };
    assert!(wanted.contains(&restyle), "{wanted:?}");
    let souls = Wanted { owner: "higgsfield.soul-2".into(), source: "higgsfield.soul_ids".into(), scope: None };
    assert!(wanted.contains(&souls), "{wanted:?}");
}
