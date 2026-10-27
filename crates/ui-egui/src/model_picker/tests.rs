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
    model.pricing = Pricing::PerUnitByChoice {
        unit: Unit::Second,
        count: Some("duration_seconds".into()),
        default_count: None,
        param: "resolution".into(),
        prices,
        input_fee: None,
    };
    assert_eq!(price_label(&model), "From 0.1 USD / second");
}

#[test]
fn workflow_metadata_never_promises_that_partner_nodes_are_free() {
    let catalog = Catalog::builtin().unwrap();
    let workflow = catalog.get(nodecraft_nodes::comfy_workflow::MODEL_ID).unwrap();
    assert_eq!(workflow.pricing, Pricing::Free, "local workflows keep their catalog rate");
    assert_eq!(price_label(workflow), "Workflow dependent; partner cost unknown");
    assert!(!catalog.card_models().any(|model| model.id == workflow.id), "the workflow is currently excluded from generic model swapping");
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

#[test]
fn azure_deployment_is_selectable_with_an_unknown_usage_price() {
    let catalog = Catalog::builtin().unwrap();
    let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Text).with_inline_prompt(), |_| Ok(())).unwrap();
    picker.set_query("Azure").unwrap();
    let state = View { catalog, picker, outcome: Outcome::Open };
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 650.0)).build_ui_state(
        |ui, state: &mut View| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
            }
        },
        state,
    );
    h.run();
    assert_eq!(price_label(h.state().catalog.get("azure-openai.deployment").unwrap()), "Usage based (USD)");
    h.get_by_label("Azure OpenAI deployment").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::Chosen("azure-openai.deployment".into()));
}

#[test]
#[ignore = "manual public-safe picker capture; AZURE45_RENDER_DIR"]
fn azure45_picker_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("AZURE45_RENDER_DIR").unwrap());
    for before in [true, false] {
        let builtin = Catalog::builtin().unwrap();
        // The only picker change is this catalog row. The before uses the exact original
        // catalog (all rows except Azure), rendered by the unchanged shipping picker.
        let catalog = Catalog::from_models(builtin.models().filter(|model| !before || model.provider != "azure-openai").cloned()).unwrap();
        let mut picker = ModelPicker::new(&catalog, &Wiring::new(Modality::Text).with_inline_prompt(), |_| Ok(())).unwrap();
        picker.set_query("Azure").unwrap();
        let state = View { catalog, picker, outcome: Outcome::Open };
        let mut h = Harness::builder()
            .wgpu()
            .with_size(egui::vec2(1000.0, 650.0))
            .build_ui_state(|ui, state: &mut View| state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK), state);
        crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
        h.run();
        h.render().unwrap().save(dir.join(if before { "azure45-before.png" } else { "azure45-after.png" })).unwrap();
    }
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

fn openai_audio39_view(before: bool, speech: bool) -> View {
    let builtin = Catalog::builtin().unwrap();
    let catalog = Catalog::from_models(builtin.models().filter(|m| !before || m.provider != "openai-audio").cloned()).unwrap();
    let wiring = if speech { Wiring::new(Modality::Audio).with_inline_prompt() } else { Wiring::new(Modality::Text).with(Modality::Audio, 1) };
    let mut picker = ModelPicker::new(&catalog, &wiring, |_| Ok(())).unwrap();
    picker.set_query("openai-audio").unwrap();
    picker.refilter(&catalog);
    View { catalog, picker, outcome: Outcome::Open }
}

#[test]
fn openai_audio39_real_picker_selects_each_modality_with_unknown_price_and_sunset() {
    for (speech, label, id) in
        [(true, "OpenAI speech (ends Jan 6, 2027)", "openai-audio.mini-tts"), (false, "OpenAI file transcription", "openai-audio.transcribe")]
    {
        let state = openai_audio39_view(false, speech);
        assert_eq!(price_label(state.catalog.get(id).unwrap()), "Usage based (USD)");
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(
            |ui, state: &mut View| {
                if state.outcome == Outcome::Open {
                    state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
                }
            },
            state,
        );
        h.run();
        h.get_by_label(label).click();
        h.run();
        assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "manual public-safe before/after picker screenshots"]
fn openai_audio39_render_picker_evidence() {
    let directory = std::env::var("AUDIO39_RENDER_DIR").expect("output directory");
    std::fs::create_dir_all(&directory).unwrap();
    for (before, speech, name) in [(true, true, "before"), (false, true, "speech"), (false, false, "transcription")] {
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).wgpu().build_ui_state(
            |ui, state: &mut View| {
                crate::theme::apply(ui.ctx(), crate::theme::ThemeKind::Dark);
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
            },
            openai_audio39_view(before, speech),
        );
        h.run();
        h.render().unwrap().save(std::path::Path::new(&directory).join(format!("audio39-{name}.png"))).unwrap();
    }
}

#[test]
fn actual_pointer_and_keyboard_choose_a_model_but_never_a_disabled_row() {
    let mut h = harness();
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

fn luma26_view(output: Modality) -> View {
    let catalog = Catalog::builtin().unwrap();
    let mut picker = ModelPicker::new(&catalog, &Wiring::new(output).with_inline_prompt(), |_| Ok(())).unwrap();
    picker.set_query("Luma").unwrap();
    picker.refilter(&catalog);
    View { catalog, picker, outcome: Outcome::Open }
}

#[test]
fn luma26_real_picker_selects_photon_and_ray_for_the_matching_output() {
    for (output, label, id) in [(Modality::Image, "Luma Photon (fal)", "fal.luma-photon"), (Modality::Video, "Luma Ray 3.2 (fal)", "fal.luma-ray32")] {
        let mut h = Harness::builder().with_size(egui::vec2(1100.0, 800.0)).build_ui_state(
            |ui, view: &mut View| {
                if view.outcome == Outcome::Open {
                    view.outcome = view.picker.show(ui.ctx(), &view.catalog, &crate::theme::DARK);
                }
            },
            luma26_view(output),
        );
        h.run();
        h.get_by_label(label).click();
        h.run();
        assert_eq!(h.state().outcome, Outcome::Chosen(id.into()));
    }
}

#[test]
#[ignore = "actual GPU capture; LUMA26_RENDER_DIR and optional LUMA26_BASELINE"]
fn luma26_visual() {
    let output = std::path::PathBuf::from(std::env::var_os("LUMA26_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let baseline = std::env::var_os("LUMA26_BASELINE").is_some();
    let mut h = Harness::builder().wgpu().with_size(egui::vec2(1100.0, 800.0)).build_ui_state(
        |ui, view: &mut View| {
            view.outcome = view.picker.show(ui.ctx(), &view.catalog, &crate::theme::DARK);
        },
        luma26_view(Modality::Image),
    );
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run_steps(4);
    assert_eq!(h.query_by_label("Luma Photon (fal)").is_some(), !baseline);
    h.render().unwrap().save(output.join(if baseline { "before.png" } else { "after.png" })).unwrap();
}
