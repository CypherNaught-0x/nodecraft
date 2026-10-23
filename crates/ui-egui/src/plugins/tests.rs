use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_engine::commands::Services;
use nodecraft_engine::plugin_cmds::{PluginHost, PluginWorker, register_plugin_service, register_plugin_worker};
use nodecraft_graph::Project;
use nodecraft_plugin::json::{self, Json};
use nodecraft_plugin_store::{MemoryBackend, Store, archive};
use nodecraft_types::ProjectId;

struct State {
    panel: PluginsPanel,
    session: Session,
}

const SHOUT: &str = r#"{"id":"user.shout","inputs":[{"kind":"text","name":"words"}],"label":"Shout","outputs":[{"expr":"upper(words)","kind":"text","name":"out"}],"schema":1,"version":1}"#;

/// A text action package file in `dir`, with `license` as its LICENSE text; its path.
fn package(dir: &std::path::Path, runtime: &str, license: &str) -> String {
    let s = |t: &str| Json::Str(t.to_owned());
    let obj = |f: Vec<(&str, Json)>| Json::Object(f.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
    let (runtime, kind) = match runtime {
        "data" => (obj(vec![("kind", s("data"))]), "catalog"),
        _ => (obj(vec![("kind", s("text_actions")), ("schema", Json::Int(1))]), "action"),
    };
    let manifest = json::canonical(&obj(vec![
        ("manifest_version", Json::Int(1)),
        ("package_id", s("io.example.text")),
        ("version", s("1.0.0")),
        ("host_protocol", obj(vec![("major", Json::Int(1)), ("min_minor", Json::Int(0)), ("max_minor", Json::Int(0))])),
        ("descriptor_contract", obj(vec![("major", Json::Int(1)), ("minor", Json::Int(0))])),
        ("license", s("MIT")),
        ("runtime", runtime),
        ("dependencies", Json::Array(vec![])),
        ("capabilities", obj(vec![("required", Json::Array(vec![])), ("optional", Json::Array(vec![]))])),
        ("registrations", Json::Array(vec![obj(vec![("id", s("warm")), ("kind", s(kind)), ("descriptor", s("d/warm.json"))])])),
    ]))
    .unwrap();
    let definition = if kind == "action" { SHOUT } else { "{}" };
    let bytes =
        archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", license.as_bytes()), ("d/warm.json", definition.as_bytes())], None).unwrap();
    let path = dir.join(format!("text-{kind}.ncraft-plugin"));
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn host() -> PluginHost {
    PluginHost { trust: None, taken: Default::default(), bindings: Default::default() }
}

fn session(services: Services) -> Session {
    Session::new(Project::new(ProjectId::from_bytes([1; 16]), "Plugins").unwrap()).unwrap().with_services(services)
}

/// A session whose plugin service answers at once.
fn direct() -> Session {
    let mut services = Services::default();
    register_plugin_service(&mut services, Store::open(MemoryBackend::default()).unwrap(), host()).unwrap();
    session(services)
}

/// A session whose plugin service answers later, from a worker, as on the desktop.
fn worker() -> Session {
    let mut services = Services::default();
    let worker = PluginWorker::spawn(|| Store::open(MemoryBackend::default()), host()).unwrap();
    register_plugin_worker(&mut services, worker).unwrap();
    session(services)
}

fn harness(session: Session) -> Harness<'static, State> {
    let panel = PluginsPanel { open: true, ..PluginsPanel::default() };
    Harness::builder()
        .with_size(egui::Vec2::new(900.0, 900.0))
        .build_ui_state(|ui, s: &mut State| s.panel.show(ui.ctx(), &mut s.session, crate::theme::ThemeKind::Dark.tokens()), State { panel, session })
}

/// Runs frames until the worker has answered everything asked.
fn settle(h: &mut Harness<'static, State>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    h.run_steps(2);
    while h.state().panel.is_waiting() {
        assert!(std::time::Instant::now() < deadline, "the plugin worker never answered");
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run_steps(1);
    }
    h.run_steps(2);
}

fn check(h: &mut Harness<'static, State>, location: &str) {
    h.state_mut().panel.location = location.to_owned();
    h.run();
    h.get_by_label("Check").click();
    settle(h);
}

/// The whole lifecycle through the panel's own buttons, against the worker the desktop uses.
#[test]
fn a_package_is_checked_installed_granted_enabled_and_uninstalled_through_the_panel() {
    let dir = tempfile::tempdir().unwrap();
    let file = package(dir.path(), "text_actions", "MIT License\n\nPermission is hereby granted, free of charge…\n");
    let mut h = harness(worker());
    settle(&mut h);
    h.get_by_label("No packages installed.");
    check(&mut h, &file);
    // The dry run shows what it is and installs nothing.
    h.get_by_label("io.example.text 1.0.0");
    h.get_by_label("Not signed: nobody vouches for who made it.");
    h.get_by_label("Licence: MIT");
    h.get_by_label("It asks for no capabilities.");
    h.get_by_label("LICENSE").click();
    h.run();
    h.get_by_label_contains("Permission is hereby granted");
    assert!(h.state().panel.rows().is_empty(), "a dry run installs nothing");
    h.get_by_label("Install").click();
    settle(&mut h);
    h.get_by_label_contains("Installed io.example.text 1.0.0");
    let row = h.state().panel.rows().first().cloned().unwrap();
    assert!(!row.enabled && row.grant.is_none(), "{row:?}");
    assert!(h.state().panel.preview().is_none());
    // Granting shows what is agreed to first.
    h.get_by_label("Grant…").click();
    settle(&mut h);
    h.get_by_label_contains("Let io.example.text run?");
    assert!(h.state().panel.rows()[0].grant.is_none(), "nothing is granted before the person agrees");
    h.get_by_label("Grant").click();
    settle(&mut h);
    assert!(h.state().panel.rows()[0].grant.is_some());
    h.get_by_label("Enable").click();
    settle(&mut h);
    assert!(h.state().panel.rows()[0].enabled);
    h.get_by_label("Revoke").click();
    settle(&mut h);
    let row = &h.state().panel.rows()[0];
    assert!(row.grant.is_none() && !row.enabled, "revoking stops it running: {row:?}");
    h.get_by_label("Uninstall").click();
    h.run();
    assert!(!h.state().panel.rows().is_empty(), "uninstalling asks first");
    h.get_by_label("Confirm uninstall").click();
    settle(&mut h);
    assert!(h.state().panel.rows().is_empty());
    h.get_by_label("No packages installed.");
    // Every answer was acknowledged: nothing is left with the worker.
    assert!(!h.state().panel.is_waiting());
}

#[test]
fn a_refused_package_says_why_and_nothing_is_installed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("broken.ncraft-plugin");
    std::fs::write(&file, b"not a package").unwrap();
    let mut h = harness(direct());
    check(&mut h, file.to_str().unwrap());
    assert!(h.state().panel.preview().is_none());
    assert!(h.state().panel.notice.is_some());
    assert!(h.state().panel.rows().is_empty());
}

#[test]
fn a_data_package_needs_no_grant_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(direct());
    check(&mut h, &package(dir.path(), "data", "MIT\n"));
    h.get_by_label("Install").click();
    settle(&mut h);
    h.get_by_label("Grant…").click();
    settle(&mut h);
    assert!(h.state().panel.consent().is_none());
    h.get_by_label_contains("it runs nothing, so it needs no grant");
    h.get_by_label("Enable").click();
    settle(&mut h);
    assert!(h.state().panel.rows()[0].enabled);
}

#[test]
fn without_a_plugin_store_the_panel_says_so() {
    let mut h = harness(session(Services::default()));
    h.run();
    h.get_by_label("Plugin packages aren't available here.");
}

#[test]
fn hostile_list_entries_and_digests_never_panic() {
    assert_eq!(short("blake3:0123456789abcdef"), "0123456789ab");
    assert_eq!(short("blake3:é"), "é");
    assert_eq!(short(""), "");
    assert!(Row::parse(&json!({"package_id": "io.example.x"})).is_none(), "a row needs a digest");
    assert!(Row::parse(&json!(null)).is_none());
    let row = Row::parse(&json!({"package_id": "io.example.x", "digest": "d", "grant": {"grant_id": 7}, "kept": [1, "k"]})).unwrap();
    assert_eq!((row.grant, row.kept), (None, vec!["k".to_owned()]));
}
