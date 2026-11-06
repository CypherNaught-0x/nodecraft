//! The LiteLLM proxy's address in the API keys panel (desktop): set, normalised, refused when
//! invalid, and cleared, through real pointer and key events.
#![cfg(not(target_arch = "wasm32"))]

use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::settings::Location;

/// The address in the app's settings store (written to disk by the store in its own time).
fn saved_url(h: &Harness<'static, NodeCraftApp>) -> Option<String> {
    h.state().store.as_ref().and_then(|store| store.get().providers.litellm_url.clone())
}

fn harness(dir: &std::path::Path) -> Harness<'static, NodeCraftApp> {
    let (store, _) = nodecraft_engine::settings::open(Location::Dir(dir.to_path_buf()));
    let mut app = NodeCraftApp::headless().unwrap();
    app.store = Some(store);
    app.keys.open = true;
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn type_address(h: &mut Harness<'static, NodeCraftApp>, text: &str) {
    h.get_by_label("Address").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(text.into()));
    h.run();
}

#[test]
fn the_keys_panel_saves_checks_and_clears_the_litellm_address() {
    let temp = tempfile::tempdir().unwrap();
    let mut h = harness(temp.path());
    assert!(h.query_by_label("Address").is_none(), "shown only for the litellm account");
    h.get_by_label("litellm").click();
    h.run();

    type_address(&mut h, "https://llms.example.com/v1/");
    h.get_by_label("Save address").click();
    h.run();
    h.run();
    assert_eq!(saved_url(&h).as_deref(), Some("https://llms.example.com"), "kept as the proxy root");
    assert!(h.state().notice.clone().unwrap_or_default().contains("Saved the LiteLLM address"));

    type_address(&mut h, "https://user:secret@llms.example.com");
    h.get_by_label("Save address").click();
    h.run();
    assert_eq!(saved_url(&h).as_deref(), Some("https://llms.example.com"), "an invalid address is never saved");
    assert!(h.query_by_label_contains("without a user name, password").is_some());

    h.get_by_label("Clear address").click();
    h.run();
    h.run();
    assert_eq!(saved_url(&h), None);
}
