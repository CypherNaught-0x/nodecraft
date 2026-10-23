//! Real memory store and private archive fixtures: no corpus path reaches a user's files.
use nodecraft_engine::{
    Session,
    commands::Services,
    plugin_cmds::{self, PluginHost},
};
use nodecraft_plugin::json::{self, Json};
use nodecraft_plugin_store::{MemoryBackend, Store, archive};
use nodecraft_types::ErrorKind;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn manifest(id: &str, version: &str) -> String {
    let s = |t: &str| Json::Str(t.to_owned());
    let obj = |f: Vec<(&str, Json)>| Json::Object(f.into_iter().map(|(k, v)| (k.to_owned(), v)).collect::<BTreeMap<_, _>>());
    json::canonical(&obj(vec![
        ("manifest_version", Json::Int(1)),
        ("package_id", s(id)),
        ("version", s(version)),
        ("host_protocol", obj(vec![("major", Json::Int(1)), ("min_minor", Json::Int(0)), ("max_minor", Json::Int(0))])),
        ("descriptor_contract", obj(vec![("major", Json::Int(1)), ("minor", Json::Int(0))])),
        ("license", s("MIT")),
        ("runtime", obj(vec![("kind", s("data"))])),
        ("dependencies", Json::Array(vec![])),
        ("capabilities", obj(vec![("required", Json::Array(vec![])), ("optional", Json::Array(vec![]))])),
        ("registrations", Json::Array(vec![obj(vec![("id", s("warm")), ("kind", s("catalog")), ("descriptor", s("d/warm.json"))])])),
    ]))
    .unwrap()
}

fn bytes(id: &str, version: &str) -> Vec<u8> {
    let manifest = manifest(id, version);
    archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/warm.json", b"{}")], None).unwrap()
}

pub fn register(session: &mut Session) -> String {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("valid.ncraft-plugin");
    let bad = dir.path().join("malformed.ncraft-plugin");
    std::fs::write(&good, bytes("io.example.other", "1.0.0")).unwrap();
    std::fs::write(&bad, b"not an archive").unwrap();
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    let old = store.import(&bytes("io.example.palette", "1.0.0"), &Default::default(), false).unwrap().entry.digest.to_string();
    store.update(&bytes("io.example.palette", "1.1.0"), &Default::default(), false).unwrap();
    let update = dir.path().join("update.ncraft-plugin");
    std::fs::write(&update, bytes("io.example.palette", "1.2.0")).unwrap();
    // The file plugins.reload installs again (1.2.0 in place of the active 1.1.0).
    store.remember_source(&nodecraft_plugin::id::PackageId::parse("io.example.palette").unwrap(), Some(update.to_str().unwrap())).unwrap();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    session
        .register_host_service(plugin_cmds::PLUGIN_SERVICE, move |request| {
            let _keep_private_directory = &dir;
            let mut request = request.clone();
            if matches!(request.get("op").and_then(Value::as_str), Some("import" | "update")) {
                // Preserve command validation. Redirect only this harness's service file acquisition:
                // `malformed` selects bounded bad bytes; every other path selects a valid fixture.
                let path = if request.get("location").and_then(Value::as_str) == Some("malformed") {
                    &bad
                } else if request.get("op").and_then(Value::as_str) == Some("update") {
                    &update
                } else {
                    &good
                };
                request["location"] = json!(path.to_str().unwrap());
            }
            plugin_cmds::handle(&mut store, &host, &request)
        })
        .unwrap();
    old
}

pub fn register_results(session: &mut Session) -> String {
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let worker = plugin_cmds::PluginWorker::spawn(|| Store::open(MemoryBackend::default()), host).unwrap();
    let mut services = Services::default();
    plugin_cmds::register_plugin_worker(&mut services, worker).unwrap();
    // Install both actual services on this fresh Session, retaining the worker in their closures.
    let project = session.document().clone();
    *session = Session::new(project).unwrap().with_services(services);
    let id = session.dispatch("plugins.list", &json!({})).unwrap()["pending"].as_str().unwrap().to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let reply = session.dispatch("plugins.result", &json!({"pending": id}));
        match reply {
            Ok(reply) if reply["status"] == "completed" => break,
            Ok(_) => (),
            Err(error) if error.kind == ErrorKind::Busy => (),
            other => panic!("unexpected memory worker result: {other:?}"),
        }
        assert!(std::time::Instant::now() < deadline, "memory worker did not complete");
        std::thread::yield_now();
    }
    id
}
