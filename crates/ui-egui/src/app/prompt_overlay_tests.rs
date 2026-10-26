//! `H` hides prompt overlays on media nodes (#321). The toggle is shell view state: never a
//! document edit, never in the undo history, never a saved setting. Only a media card obeys it —
//! a text card's composer is its editor, not an overlay over a result — and `H` is a plan §2.1
//! canvas letter, so it yields to a focused text field, an open popup, the palette and the "Turn
//! into" menu, and to a gesture in flight, exactly as the quick-add letters do. Everything here
//! runs against public mock models; nothing calls a real provider.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::{
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, RunCatalog},
};

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const N: NodeId = NodeId::from_bytes([92; 16]);

/// A public mock image model: a prompt, a variation count and one other parameter, so its card
/// has a complete prompt overlay over a media result.
const MOCK_IMAGE: &str = r#"
[[model]]
id = "mock-square"
provider = "mock"
provider_model = "mock-image"
label = "Mock image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "n"
label = "Images"
control = { type = "unsigned", min = "1", max = "4" }
default = { type = "unsigned", value = "2" }
[[model.params.params]]
name = "aspect"
label = "Aspect"
control = { type = "choice", options = [{ value = "1:1", label = "1:1" }, { value = "16:9", label = "16:9" }] }
default = { type = "text", value = "1:1" }
[model.parameter_contract]
version = { major = 1, minor = 0 }
namespace = "mock-square"
owner = "model"
[[model.parameter_contract.rules]]
param = "n"
unit = "count"
meaning = "generated_image_count"
[[model.parameter_contract.schema.params]]
name = "n"
label = "Images"
control = { type = "unsigned", min = "1", max = "4" }
default = { type = "unsigned", value = "2" }
[[model.parameter_contract.schema.params]]
name = "aspect"
label = "Aspect"
control = { type = "choice", options = [{ value = "1:1", label = "1:1" }, { value = "16:9", label = "16:9" }] }
default = { type = "text", value = "1:1" }
"#;

/// A public mock text model: the same prompt and arrow over a text result, with no variation
/// counter because one run makes one answer.
const MOCK_TEXT: &str = r#"
[[model]]
id = "mock-writer"
provider = "mock"
provider_model = "mock-text"
label = "Mock writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A harness over one generation card of `kind`, made from `model`, whose prompt says `prompt`.
fn scene(kind: &str, model: &str, prompt: &str) -> Harness<'static, NodeCraftApp> {
    let toml = if kind == nodecraft_nodes::generation::TEXT_KIND { MOCK_TEXT } else { MOCK_IMAGE };
    let catalog = nodecraft_catalog::Catalog::from_toml(toml).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([93; 16]), "Prompt overlay").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(kind)
        .unwrap()
        .instantiate_instance(N, Default::default(), &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text(prompt.into()))].into())
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 260.0, y: 160.0 };
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: nodecraft_exec::cost::Budgets::default(), spent: nodecraft_exec::cost::Spent::default() });
    // The mock model prices and runs through the mock provider, so Enter on a focused card quotes
    // a real price here exactly as it does on a person's card.
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(std::sync::Arc::new(nodecraft_provider_testkit::mock::MockProvider::standard())).unwrap();
    routing.select(N, "mock", binding.target()).unwrap();
    let mut session =
        Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(Services::default());
    session.set_provider_routing(routing).unwrap();
    let app = NodeCraftApp::from_session(session, C).unwrap();
    let mut h = Harness::builder()
        .with_size(Vec2::new(1100.0, 800.0))
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

/// An image card: a media card, so `H` hides its overlay.
fn image() -> Harness<'static, NodeCraftApp> {
    scene(nodecraft_nodes::generation::KIND, "mock-square", "A red square")
}

/// A text card: not a media card, so `H` leaves its prompt alone.
fn text() -> Harness<'static, NodeCraftApp> {
    scene(nodecraft_nodes::generation::TEXT_KIND, "mock-writer", "A haiku about rain")
}

/// The three widgets the prompt overlay is made of, in reading order: the prompt, its variation
/// counter and its submit arrow (#83). They are real widgets on the card, so this says what the
/// card actually drew rather than what the shell's flag holds.
fn overlay(h: &Harness<'static, NodeCraftApp>) -> [bool; 3] {
    [
        h.query_by_role_and_label(Role::TextInput, "Prompt").is_some(),
        h.query_by_role_and_label(Role::Button, "Fewer images").is_some(),
        h.query_by_role_and_label(Role::Button, "Generate").is_some(),
    ]
}

fn node(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

fn map(h: &Harness<'static, NodeCraftApp>) -> Rect {
    crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map
}

fn key(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE }
}
fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}
fn raw(h: &mut Harness<'static, NodeCraftApp>, events: impl IntoIterator<Item = egui::Event>) {
    h.input_mut().events.extend(events);
    h.step();
}
fn double_click(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
    h.hover_at(pos);
    h.run();
    for _ in 0..2 {
        raw(h, [pointer(pos, true), pointer(pos, false)]);
    }
    h.run();
}

/// One press and its release: what a platform delivers for a single tap of `H`.
fn press_h(h: &mut Harness<'static, NodeCraftApp>) {
    h.key_press(egui::Key::H);
    h.run();
}

/// A letter as a platform delivers it to a focused text field: the key, then the character.
fn type_h(h: &mut Harness<'static, NodeCraftApp>) {
    h.event(key(egui::Key::H, true));
    h.event(egui::Event::Text("h".into()));
    h.event(key(egui::Key::H, false));
    h.run();
}

/// Whether a press of `H` is still sitting in the input, unconsumed by the canvas.
fn h_unconsumed(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.ctx.input(|i| i.events.iter().any(|event| event == &key(egui::Key::H, true)))
}

#[test]
fn the_overlay_shows_on_a_media_card_and_h_toggles_it_both_ways() {
    let mut h = image();
    assert!(h.state().prompt_overlays, "overlays start shown");
    assert_eq!(overlay(&h), [true, true, true], "the prompt, its counter and its arrow are the overlay");
    press_h(&mut h);
    assert!(!h.state().prompt_overlays);
    assert_eq!(overlay(&h), [false, false, false], "one press hides all of it: the card renders differently");
    press_h(&mut h);
    assert!(h.state().prompt_overlays);
    assert_eq!(overlay(&h), [true, true, true], "and the next press brings it back");
}

#[test]
fn h_shortcut_dispatches_a_command_instead_of_flipping_view_state_inline() {
    let mut h = image();
    // Session records successful dispatches independently of shell view changes. An inline
    // flag flip still renders correctly, but does not produce this command observation.
    h.state_mut().session.perf_mut().set_enabled(true);
    h.state_mut().session.perf_mut().clear();
    press_h(&mut h);
    let dispatched = h
        .state()
        .session
        .perf()
        .report()
        .metrics
        .into_iter()
        .find(|metric| metric.metric == nodecraft_engine::perf::Metric::Dispatch)
        .map(|metric| metric.recorded);
    assert_eq!(dispatched, Some(1), "H must successfully dispatch the navigation command");
    assert_eq!(overlay(&h), [false, false, false]);
}

#[test]
fn navigation_service_family_is_not_in_the_unscoped_palette() {
    let mut h = image();
    let entries = h.state().palette_entries();
    for id in ["canvas.minimap_toggle", "canvas.prompt_overlays_toggle"] {
        assert!(h.state().session.registry().get(id).is_ok(), "{id} is registered");
        assert!(h.state().session.enabled(id, &json!({})).is_err(), "{id} lacks scoped navigation context");
        assert!(
            !entries.iter().any(|entry| matches!(&entry.target, palette::Target::Command(command) if *command == id)),
            "{id} cannot be offered by the unscoped palette filter"
        );
        h.state_mut().dispatch_palette_command(id);
        assert!(h.state().notice.is_some(), "raw palette dispatch also refuses {id}");
    }
    assert!(h.state().navigation.visible, "minimap stayed visible");
    assert!(h.state().prompt_overlays, "overlays stayed visible");
}

#[test]
fn scoped_command_toggles_the_actual_image_overlay_without_document_edits() {
    let mut h = image();
    let ctx = h.ctx.clone();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    assert_eq!(overlay(&h), [true, true, true]);
    assert!(h.state_mut().navigation_enabled(&ctx, "canvas.prompt_overlays_toggle", &json!({})).is_ok());
    let response = h.state_mut().dispatch_navigation(&ctx, "canvas.prompt_overlays_toggle", &json!({})).unwrap();
    assert_eq!(response["prompt_overlays_visible"], false);
    h.run();
    assert_eq!(overlay(&h), [false, false, false]);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.prompt_overlays_toggle", &json!({"unexpected":true})).is_err());
    assert!(!h.state().prompt_overlays, "invalid params leave the view untouched");
    h.state_mut().dispatch_navigation(&ctx, "canvas.prompt_overlays_toggle", &json!({})).unwrap();
    h.run();
    assert_eq!(overlay(&h), [true, true, true]);
}

#[test]
fn hiding_the_overlay_leaves_the_document_revision_and_history_untouched() {
    let mut h = image();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let events = h.state().session.events_since(0).unwrap();
    let size = card::world_size(node(h.state()), None, h.state().theme.tokens());
    press_h(&mut h);
    assert!(!h.state().prompt_overlays, "the view state moved");
    assert_eq!(h.state().session.document(), &document, "the document is untouched");
    assert_eq!(h.state().session.revision(), revision, "no revision, so nothing autosaves");
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history, "nothing entered the undo history");
    assert_eq!(h.state().session.events_since(0).unwrap(), events, "and no event was published");
    press_h(&mut h);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    // The board must not jump: the card keeps the size its kind declares, overlay or not.
    assert_eq!(card::world_size(node(h.state()), None, h.state().theme.tokens()), size, "the card is the same size either way");
}

#[test]
fn hiding_the_overlay_hides_the_overlay_and_nothing_else_on_the_card() {
    let mut h = image();
    assert!(h.query_by_role_and_label(Role::ComboBox, "Aspect").is_some(), "the card's own parameters are visible");
    assert!(h.query_by_role_and_label(Role::Button, "Rename A RED SQUARE").is_some(), "and the card itself is there");
    press_h(&mut h);
    assert_eq!(overlay(&h), [false, false, false], "the overlay is gone");
    assert!(h.query_by_role_and_label(Role::ComboBox, "Aspect").is_some(), "its parameter stays");
    assert!(h.query_by_role_and_label(Role::Button, "Rename A RED SQUARE").is_some(), "so does the card");
    // Hiding is painting only: Enter on the focused card still asks to run (#83), with no arrow
    // on the card to click.
    h.get_by_role_and_label(Role::Button, "Rename A RED SQUARE").focus();
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().run_confirm.is_some(), "Enter still opens the run's price confirmation with the overlay hidden");
}

#[test]
fn a_text_card_keeps_its_prompt_and_arrow_while_the_toggle_is_off() {
    let mut h = text();
    // A text card has a prompt and an arrow, and no counter: this model makes one answer.
    assert_eq!(overlay(&h), [true, false, true], "a text card starts with its prompt and its arrow");
    press_h(&mut h);
    assert!(!h.state().prompt_overlays, "the view state is global and did move");
    assert_eq!(overlay(&h), [true, false, true], "and a text card ignores it: that prompt is its editor");
    press_h(&mut h);
    assert_eq!(overlay(&h), [true, false, true]);
}

#[test]
fn h_in_the_cards_prompt_field_types_the_letter_and_does_not_toggle() {
    let mut h = image();
    // A real click into the prompt field, as a person typing a capital H would.
    h.get_by_role_and_label(Role::TextInput, "Prompt").click();
    h.run();
    assert!(canvas::text_input_focused(&h.ctx), "the prompt field owns focus");
    type_h(&mut h);
    assert!(h.state().prompt_overlays, "`H` stayed with the field");
    assert_eq!(&overlay(&h)[1..], &[true, true], "the rest of the overlay is still on the card");
    // The prompt's preview becomes a real multiline editor once it is clicked.
    let editor = h.get_by_role_and_label(Role::MultilineTextInput, "Prompt");
    let typed = editor.accesskit_node().value().map(|value| value.to_string()).unwrap_or_default();
    assert!(typed.ends_with('h'), "the letter reached the prompt: {typed:?}");
    assert_eq!(node(h.state()).params.get("prompt"), Some(&Value::Text("A red square".into())), "the typed draft is not yet a document edit");
}

#[test]
fn h_in_the_search_field_types_the_letter_and_does_not_toggle() {
    let mut h = image();
    let document = h.state().session.document().clone();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
    h.run();
    assert!(h.state().search.open, "search opened and took focus");
    type_h(&mut h);
    assert!(h.state().prompt_overlays, "`H` stayed with the search field");
    assert_eq!(overlay(&h), [true, true, true], "so the media card keeps its overlay");
    assert_eq!(h.state().search.draft, "h", "the letter reached the query");
    assert_eq!(h.state().session.document(), &document);
}

#[test]
fn an_open_popup_owns_h_until_it_closes() {
    let mut h = image();
    // The card's own Aspect menu. Its items take no letters, so an unconsumed `H` would otherwise
    // reach the board behind a menu the person is looking at.
    h.get_by_role_and_label(Role::ComboBox, "Aspect").click();
    h.run();
    assert!(egui::Popup::is_any_open(&h.ctx), "the aspect menu is open");
    assert!(!canvas::text_input_focused(&h.ctx), "and nothing is typing, so only the popup can own the key");
    press_h(&mut h);
    assert!(h.state().prompt_overlays, "`H` belongs to the open popup, not to the board");
    assert_eq!(overlay(&h), [true, true, true], "the overlay is still drawn");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!egui::Popup::is_any_open(&h.ctx), "Escape closed the menu");
    press_h(&mut h);
    assert!(!h.state().prompt_overlays, "with nothing open, `H` toggles");
}

#[test]
fn h_typed_while_the_palette_is_open_goes_to_its_query_and_not_to_the_board() {
    let mut h = image();
    double_click(&mut h, Pos2::new(150.0, 620.0));
    h.run();
    assert!(h.state().palette.is_some(), "a double-click on empty canvas opened the palette");
    // The palette re-claims focus for its query whenever nothing else has it, and the shell also
    // refuses every canvas letter while it is open (the same lock quick-add uses), so a letter
    // cannot reach the board from the palette either way.
    type_h(&mut h);
    assert!(h.state().prompt_overlays, "`H` went to the palette, which picks kinds with letters");
    assert_eq!(h.state().palette.as_ref().map(|palette| palette.query.as_str()), Some("h"), "the letter reached the query");
    assert_eq!(overlay(&h), [true, true, true], "so the media card keeps its overlay");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().palette.is_none(), "Escape closes the palette");
    press_h(&mut h);
    assert!(!h.state().prompt_overlays, "and once it is gone the canvas takes the letter");
}

#[test]
fn a_marquee_in_flight_owns_h_until_it_is_released() {
    let mut h = image();
    let at = Pos2::new(140.0, 640.0);
    let to = at + Vec2::new(60.0, 40.0);
    let drag = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT };
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    raw(&mut h, [egui::Event::PointerMoved(at), drag]);
    raw(&mut h, [egui::Event::PointerMoved(to)]);
    assert!(h.state().marquee.is_some(), "a shift-drag is boxing a selection");
    raw(&mut h, [key(egui::Key::H, true)]);
    assert!(h.state().prompt_overlays, "a letter during a marquee changes nothing");
    assert!(h_unconsumed(&h), "and stays unconsumed for whoever does own it");
    raw(&mut h, [key(egui::Key::H, false), pointer(to, false), egui::Event::ModifiersChanged(egui::Modifiers::NONE)]);
    assert!(h.state().marquee.is_none(), "the marquee is over");
    press_h(&mut h);
    assert!(!h.state().prompt_overlays, "a fresh press after the gesture toggles");
}

#[test]
fn a_held_minimap_owns_h_until_the_pointer_is_released() {
    let mut h = image();
    let at = map(&h).center();
    raw(&mut h, [egui::Event::PointerMoved(at), pointer(at, true)]);
    assert!(h.state().navigation.pointer_owned, "the minimap owns this press");
    assert!(h.state().navigation.drag.is_some(), "and is dragging the canvas");
    raw(&mut h, [key(egui::Key::H, true)]);
    assert!(h.state().prompt_overlays, "a key during a held map gesture changes nothing");
    assert!(h_unconsumed(&h), "and stays unconsumed");
    raw(&mut h, [key(egui::Key::H, false)]);
    raw(&mut h, [pointer(at, false)]);
    assert!(!h.state().navigation.pointer_owned, "the map let go");
    press_h(&mut h);
    assert!(!h.state().prompt_overlays, "a fresh press once the map is released toggles");
}

#[test]
fn a_held_down_h_toggles_once_and_never_strobes() {
    let mut h = image();
    // A platform sends one press and then repeats it while the key is held; egui marks those
    // repeats itself, from the key it still believes is down.
    raw(&mut h, [key(egui::Key::H, true)]);
    assert!(!h.state().prompt_overlays, "the fresh press hid the overlay");
    for _ in 0..3 {
        raw(&mut h, [key(egui::Key::H, true)]);
        assert!(!h.state().prompt_overlays, "a repeat of the held key does not bring it back");
    }
    assert!(!h_unconsumed(&h), "the repeats were consumed, not left for someone else");
    raw(&mut h, [key(egui::Key::H, false)]);
    press_h(&mut h);
    assert!(h.state().prompt_overlays, "released and pressed again, it shows");
}

#[test]
fn shift_h_toggles_like_plain_h_because_that_is_how_a_capital_is_typed() {
    let mut h = image();
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::H);
    h.run();
    assert!(!h.state().prompt_overlays, "Shift+H is the same letter");
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::H);
    h.run();
    assert!(h.state().prompt_overlays, "both ways, both directions");
    // A modifier that means something else is not this letter.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::H);
    h.run();
    assert!(h.state().prompt_overlays, "Cmd+H is not the toggle");
}

#[test]
fn h_creates_selects_and_deletes_nothing() {
    let mut h = image();
    let document = h.state().session.document().clone();
    let selection = h.state().selection.clone();
    let revision = h.state().session.revision();
    press_h(&mut h);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().selection, selection, "and the selection is not view state it disturbs");
    assert_eq!(h.state().session.document().canvases().get(&C).unwrap().nodes().len(), 1, "no quick-added node");
}

/// Local public-safe visual evidence, selected explicitly with an output directory.
#[test]
#[ignore = "manual local screenshot capture; requires PROMPT_OVERLAY321_RENDER_ROOT"]
fn render_prompt_overlay321_toggle() {
    let root = std::path::PathBuf::from(std::env::var("PROMPT_OVERLAY321_RENDER_ROOT").unwrap());
    let mut h = image();
    assert_eq!(overlay(&h), [true, true, true]);
    h.render().unwrap().save(root.join("overlay-shown.png")).unwrap();
    press_h(&mut h);
    assert_eq!(overlay(&h), [false, false, false]);
    h.render().unwrap().save(root.join("overlay-hidden.png")).unwrap();
    press_h(&mut h);
    assert_eq!(overlay(&h), [true, true, true]);
    h.render().unwrap().save(root.join("overlay-shown-again.png")).unwrap();
}
