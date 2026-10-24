use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_catalog::Pricing;
use nodecraft_types::Currency;
use std::cell::Cell;

fn model(id: &str, provider: &str, label: &str, output: &str) -> String {
    format!(
        r#"
[[model]]
id = "{id}"
provider = "{provider}"
provider_model = "remote-{id}"
label = "{label}"
description = "Public-safe catalog fixture"
latency_s = 4
quality = 3
[model.pricing]
type = "per_call"
price = "0.04 USD"
[[model.modes]]
id = "generate"
output = "{output}"
[[model.modes.inputs]]
role = "prompt"
modality = "text"
min = 1
max = 1
inline = true
"#
    )
}
fn catalog() -> Catalog {
    Catalog::from_toml(
        &[
            model("a.good", "a", "Alpha", "image"),
            model("a.blocked", "a", "Reference model", "image"),
            model("a.audio", "a", "Audio", "audio"),
            model("b.good", "b", "Beta", "image"),
        ]
        .concat(),
    )
    .unwrap()
}
fn picker(catalog: &Catalog) -> ModelPicker {
    ModelPicker::new(catalog, &Wiring::new(Modality::Image).with_inline_prompt(), |m| {
        if m.id == "a.blocked" { Err("The selected role is not supported".into()) } else { Ok(()) }
    })
    .unwrap()
}
fn selected(picker: &ModelPicker) -> Option<&str> {
    picker.highlighted.and_then(|i| picker.choices.get(i)).map(|c| c.id.as_str())
}

#[test]
fn groups_keep_disabled_models_and_run_additional_checks_only_once() {
    let catalog = catalog();
    let calls = Cell::new(0);
    let picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Image).with_inline_prompt(), |m| {
        calls.set(calls.get() + 1);
        if m.id == "a.blocked" { Err("The selected role is not supported".into()) } else { Ok(()) }
    })
    .unwrap();
    assert_eq!(calls.get(), 3, "coarsely incompatible audio never reaches the role predicate");
    assert_eq!(picker.choices.len(), 4, "disabled candidates remain present");
    assert_eq!(selected(&picker), Some("a.good"));
    assert_eq!(picker.rows.iter().filter(|r| matches!(r, Row::Provider(_))).count(), 2);
    assert!(picker.choices.iter().find(|c| c.id == "a.blocked").unwrap().unavailable.as_ref().unwrap().contains("role"));
    assert!(picker.choices.iter().find(|c| c.id == "a.audio").unwrap().unavailable.is_some());
    assert_eq!(picker.retained_text_bytes(), picker.choices.iter().map(|c| c.id.len() + c.unavailable.as_ref().map_or(0, String::len)).sum::<usize>());
}

#[test]
fn search_is_case_insensitive_bounded_and_keyboard_skips_disabled_rows() {
    let catalog = catalog();
    let mut picker = picker(&catalog);
    // The disabled-row assertions below need them visible; the default hides them (see below).
    picker.selectable_only = false;
    picker.refilter(&catalog);
    picker.move_highlight(true);
    assert_eq!(selected(&picker), Some("b.good"));
    picker.move_highlight(false);
    assert_eq!(selected(&picker), Some("a.good"));
    picker.set_query("BeTA").unwrap();
    picker.refilter(&catalog);
    assert_eq!(picker.filtered.len(), 1);
    assert_eq!(selected(&picker), Some("b.good"));
    picker.set_query("a.blocked").unwrap();
    picker.refilter(&catalog);
    assert_eq!(picker.filtered.len(), 1);
    assert_eq!(selected(&picker), None, "search does not turn an incompatible row into a choice");
    picker.set_query(&"λ".repeat(128)).unwrap();
    assert!(picker.set_query(&"λ".repeat(129)).is_err());
}

#[test]
fn selectable_only_is_on_by_default_and_toggling_it_off_reveals_disabled_models() {
    let catalog = catalog();
    let mut picker = picker(&catalog);
    assert!(picker.selectable_only);
    let visible = |picker: &ModelPicker| picker.filtered.iter().map(|i| picker.choices[*i].id.clone()).collect::<Vec<_>>();
    assert_eq!(visible(&picker), ["a.good", "b.good"], "disabled models are hidden by default");
    assert_eq!(selected(&picker), Some("a.good"));
    picker.selectable_only = false;
    picker.refilter(&catalog);
    assert!(visible(&picker).contains(&"a.blocked".to_owned()), "turning the filter off shows disabled models again");
    assert_eq!(selected(&picker), Some("a.good"), "highlight still lands on a selectable model");
    picker.selectable_only = true;
    picker.refilter(&catalog);
    assert_eq!(visible(&picker), ["a.good", "b.good"]);
}

#[test]
fn reasons_refuse_oversize_instead_of_retaining_unbounded_diagnostics() {
    let catalog = catalog();
    assert!(ModelPicker::new(&catalog, &Wiring::new(Modality::Image).with_inline_prompt(), |_| Err("x".repeat(MAX_TEXT + 1))).is_err());
}

#[test]
fn model_rates_distinguish_free_usage_and_unit_prices_without_a_run_quote() {
    let catalog = catalog();
    let mut model = catalog.get("a.good").unwrap().clone();
    assert_eq!(price_label(&model), "0.04 USD / call");
    model.pricing = Pricing::Free;
    assert_eq!(price_label(&model), "Free");
    model.pricing = Pricing::ByUsage { currency: Currency::USD };
    assert_eq!(price_label(&model), "Usage based (USD)");
    model.pricing = Pricing::PerUnit { unit: Unit::ThousandTokens, price: "0.002 USD".parse().unwrap(), count: None, default_count: None };
    assert_eq!(price_label(&model), "0.002 USD / 1,000 tokens");
    assert_eq!(amount(Money::new(Currency::USD, 1)), "0.000001 USD");
    // Priced per second of source (#179): the cheapest rate, since the setting picks one.
    let prices = [("480p", "0.318 USD"), ("720p", "0.681 USD")].map(|(k, v)| (k.to_owned(), v.parse().unwrap())).into();
    model.pricing = Pricing::PerSourceSecond { param: "resolution".into(), prices };
    assert_eq!(price_label(&model), "From 0.318 USD / second of source");
    // Priced per second of output by its resolution (#742).
    let prices = [("720p", "0.10 USD"), ("1080p", "0.12 USD")].map(|(k, v)| (k.to_owned(), v.parse().unwrap())).into();
    model.pricing =
        Pricing::PerUnitByChoice { unit: Unit::Second, count: Some("duration_seconds".into()), default_count: None, param: "resolution".into(), prices };
    assert_eq!(price_label(&model), "From 0.1 USD / second");
}

#[test]
fn a_source_edit_says_what_it_does_to_the_source() {
    let catalog = Catalog::builtin().unwrap();
    let swap = catalog.get("higgsfield.genjutsu-object-swap").unwrap().clone();
    assert_eq!(price_label(&swap), "From 0.318 USD / second of source");
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 600.0)).build_ui(move |ui| model_details(ui, &swap));
    h.run();
    h.get_by_label("Object swap of source; sources of at least 4 s, only the first 30 s is used; sound not documented");
}

struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}
fn harness() -> Harness<'static, View> {
    let catalog = catalog();
    let state = View { picker: picker(&catalog), catalog, outcome: Outcome::Open };
    Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
        |ui, state: &mut View| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
            }
        },
        state,
    )
}

#[test]
fn actual_pointer_and_keyboard_choose_a_model_but_never_a_disabled_row() {
    let mut h = harness();
    h.run();
    h.get_by_label("Selectable only").click();
    h.run();
    let blocked = h.get_by_label("Reference model").rect().center();
    for pressed in [true, false] {
        h.input_mut().events.extend([
            egui::Event::PointerMoved(blocked),
            egui::Event::PointerButton { pos: blocked, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE },
        ]);
        // A held pointer deliberately keeps an egui Area repainting. Advance the actual
        // press/release frames rather than asking the harness to settle while held.
        h.step();
    }
    assert_eq!(h.state().outcome, Outcome::Open);
    h.get_by_label("Alpha").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::Chosen("a.good".into()));
    let mut h = harness();
    h.run();
    h.key_press(egui::Key::ArrowDown);
    h.run();
    assert!(h.query_by_label("Beta").is_some(), "keyboard movement reveals the virtualized row");
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().outcome, Outcome::Chosen("b.good".into()));
}

#[test]
fn a_fresh_query_and_enter_use_this_frames_matches_and_escape_closes() {
    let mut h = harness();
    h.run();
    h.input_mut().events.extend([
        egui::Event::Text("Beta".into()),
        egui::Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::Enter, physical_key: Some(egui::Key::Enter), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE },
    ]);
    h.run();
    assert_eq!(h.state().outcome, Outcome::Chosen("b.good".into()));
    let mut h = harness();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().outcome, Outcome::Closed);
}
