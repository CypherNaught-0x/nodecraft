//! The server section's rows (#970): placed with the catalog's ComfyUI cards, searched with
//! them, and chosen only when available.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

fn model(id: &str, provider: &str, label: &str) -> String {
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
type = "free"
[[model.modes]]
id = "generate"
output = "image"
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
    Catalog::from_toml(&[model("a.alpha", "a", "Alpha"), model("comfyui.local", "comfyui", "Local"), model("z.zeta", "z", "Zeta")].concat()).unwrap()
}

fn section() -> ServerSection {
    let entry = |group: &str, name: &str, unavailable: Option<&str>| ServerEntry {
        group: group.into(),
        name: name.into(),
        detail: "Runs as Local with this checkpoint".into(),
        unavailable: unavailable.map(str::to_owned),
    };
    ServerSection {
        server: Some("studio".into()),
        read: true,
        can_refresh: true,
        error: None,
        missing: vec!["gone.safetensors".into()],
        groups: vec![
            ServerGroup { label: "Checkpoints".into(), entries: vec![entry("checkpoints", "juggernaut.safetensors", None)], more: 0 },
            ServerGroup { label: "LoRAs".into(), entries: vec![entry("loras", "detail.safetensors", Some("A LoRA can't run on its own."))], more: 3 },
        ],
        workflows: vec![ServerWorkflow {
            canvas: CanvasId::from_bytes([1; 16]),
            id: NodeId::from_bytes([2; 16]),
            title: "Fox portrait".into(),
            unavailable: None,
        }],
    }
}

fn picker(catalog: &Catalog) -> ModelPicker {
    ModelPicker::for_targets(catalog, |_| Ok(())).unwrap().with_server(section(), catalog)
}

#[test]
fn server_rows_follow_the_catalogs_comfyui_cards() {
    let catalog = catalog();
    let picker = picker(&catalog);
    let kinds: Vec<&str> = picker
        .rows
        .iter()
        .map(|row| match row {
            Row::Provider(_) => "provider",
            Row::Model(_) => "model",
            Row::Server => "server",
            Row::Missing(_) => "missing",
            Row::ServerGroup(_) => "group",
            Row::ServerModel(..) => "entry",
            Row::Workflows => "workflows",
            Row::Workflow(_) => "workflow",
        })
        .collect();
    assert_eq!(
        kinds,
        ["provider", "model", "provider", "model", "server", "missing", "group", "entry", "group", "entry", "workflows", "workflow", "provider", "model"],
        "a, comfyui with the server's models, then z"
    );
    assert!(picker.retained_text_bytes() > "juggernaut.safetensors".len());
}

#[test]
fn search_narrows_the_server_rows_too() {
    let catalog = catalog();
    let mut picker = picker(&catalog);
    picker.set_query("detail").unwrap();
    picker.refilter(&catalog);
    let entries: Vec<_> = picker.rows.iter().filter(|row| matches!(row, Row::ServerModel(..) | Row::Workflow(_))).collect();
    assert_eq!(entries.len(), 1, "only the LoRA matches");
    assert!(matches!(entries[0], Row::ServerModel(1, 0)));
}

struct View {
    catalog: Catalog,
    picker: ModelPicker,
    outcome: Outcome,
}

/// The picker searched for `query`, so the rows it tests are in view.
fn harness(query: &str) -> Harness<'static, View> {
    let catalog = catalog();
    let mut picker = picker(&catalog);
    picker.set_query(query).unwrap();
    let state = View { picker, catalog, outcome: Outcome::Open };
    Harness::builder().with_size(egui::vec2(900.0, 1400.0)).build_ui_state(
        |ui, state: &mut View| {
            if state.outcome == Outcome::Open {
                state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
            }
        },
        state,
    )
}

#[test]
fn pointer_chooses_an_available_entry_and_never_an_unavailable_one() {
    let mut h = harness("detail");
    h.run();
    h.get_by_label("From ComfyUI server \"studio\"");
    h.get_by_label("Missing on the server: gone.safetensors");
    h.get_by_label("3 more not listed");
    h.get_by_label("detail.safetensors").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::Open, "an unavailable LoRA is never chosen");

    let mut h = harness("juggernaut");
    h.run();
    h.get_by_label("juggernaut.safetensors").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::ServerModel { group: "checkpoints".into(), name: "juggernaut.safetensors".into() });

    let mut h = harness("fox");
    h.run();
    h.get_by_label("Fox portrait").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::Workflow { canvas: CanvasId::from_bytes([1; 16]), id: NodeId::from_bytes([2; 16]) });

    let mut h = harness("nothing matches this");
    h.run();
    h.get_by_label("Refresh server models").click();
    h.run();
    assert_eq!(h.state().outcome, Outcome::RefreshServer);
}

#[test]
fn arrow_keys_and_enter_reach_the_server_rows() {
    let mut h = harness("");
    h.run();
    h.key_press(egui::Key::ArrowDown);
    h.run();
    h.key_press(egui::Key::ArrowDown);
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(
        h.state().outcome,
        Outcome::ServerModel { group: "checkpoints".into(), name: "juggernaut.safetensors".into() },
        "Alpha, Local, then the server's"
    );

    let mut h = harness("juggernaut");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().outcome, Outcome::ServerModel { group: "checkpoints".into(), name: "juggernaut.safetensors".into() }, "the only match is highlighted");
}

#[test]
fn a_list_that_cant_be_read_is_shown_in_the_heading() {
    let catalog = catalog();
    let failed = ServerSection {
        server: None,
        read: false,
        can_refresh: false,
        error: Some("the list is too large".into()),
        missing: Vec::new(),
        groups: Vec::new(),
        workflows: Vec::new(),
    };
    let mut picker = ModelPicker::for_targets(&catalog, |_| Ok(())).unwrap().with_server(failed, &catalog);
    picker.set_query("nothing matches this").unwrap();
    let state = View { picker, catalog, outcome: Outcome::Open };
    let mut h = Harness::builder().with_size(egui::vec2(900.0, 1400.0)).build_ui_state(
        |ui, state: &mut View| {
            state.outcome = state.picker.show(ui.ctx(), &state.catalog, &crate::theme::DARK);
        },
        state,
    );
    h.run();
    h.get_by_label("From your ComfyUI server");
    h.get_by_label("Couldn't list the server's models: the list is too large");
}
