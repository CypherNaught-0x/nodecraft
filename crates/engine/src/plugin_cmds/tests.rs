use super::*;
use crate::Session;
use nodecraft_plugin::json::{self, Json};
use nodecraft_plugin_store::{MemoryBackend, archive};
use std::collections::BTreeMap;

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

/// Writes a package file into `dir`; its path.
fn package(dir: &std::path::Path, id: &str, version: &str) -> String {
    let manifest = manifest(id, version);
    let bytes = archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/warm.json", b"{}")], None).unwrap();
    let path = dir.join(format!("{id}-{version}.ncraft-plugin"));
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn session(taken: BTreeSet<String>, trust: Option<PathBuf>) -> Session {
    let mut services = Services::default();
    let store = Store::open(MemoryBackend::default()).unwrap();
    register_plugin_service(&mut services, store, PluginHost { trust, taken, bindings: BTreeSet::new() }).unwrap();
    Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap().with_services(services)
}

#[test]
fn packages_go_through_the_whole_lifecycle_without_touching_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let before = (s.document().clone(), s.revision());
    let v1 = package(dir.path(), "io.example.palette", "1.0.0");
    let dry = s.dispatch("plugin.import", &json!({"location": v1, "dry_run": true})).unwrap();
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["provenance"], "unsigned");
    assert_eq!(dry["registrations"], json!(["plugin.io.example.palette.warm"]));
    assert_eq!(s.dispatch("plugin.list", &json!({})).unwrap(), json!({"packages": []}), "a dry run installs nothing");
    s.dispatch("plugin.import", &json!({"location": v1})).unwrap();
    let list = s.dispatch("plugin.list", &json!({})).unwrap();
    assert_eq!(list["packages"][0]["state"], "disabled");
    let enabled = s.dispatch("plugin.enable", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(enabled, json!({"registered": ["plugin.io.example.palette.warm"], "unbound": []}));
    assert_eq!(
        s.dispatch("plugin.update", &json!({"location": package(dir.path(), "io.example.palette", "1.1.0")})).unwrap_err().kind,
        ErrorKind::Conflict,
        "disable first"
    );
    s.dispatch("plugin.disable", &json!({"package_id": "io.example.palette"})).unwrap();
    let updated = s.dispatch("plugin.update", &json!({"location": package(dir.path(), "io.example.palette", "1.1.0")})).unwrap();
    let old = updated["replaced"].as_str().unwrap().to_owned();
    assert_eq!(s.dispatch("plugin.list", &json!({})).unwrap()["packages"][0]["kept"], json!([old.clone()]));
    s.dispatch("plugin.rollback", &json!({"package_id": "io.example.palette", "digest": old})).unwrap();
    assert_eq!(s.dispatch("plugin.list", &json!({})).unwrap()["packages"][0]["version"], "1.0.0");
    let gone = s.dispatch("plugin.uninstall", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(gone["removed"].as_array().unwrap().len(), 2);
    assert_eq!((s.document().clone(), s.revision()), before, "no document change");
    assert!(!s.can_undo(), "no undo step");
}

#[test]
fn bad_requests_are_errors_the_person_can_act_on() {
    let dir = tempfile::tempdir().unwrap();
    // A host ID a package would take.
    let mut s = session(BTreeSet::from(["plugin.io.example.palette.warm".to_owned()]), None);
    s.dispatch("plugin.import", &json!({"location": package(dir.path(), "io.example.palette", "1.0.0")})).unwrap();
    assert_eq!(s.dispatch("plugin.enable", &json!({"package_id": "io.example.palette"})).unwrap_err().kind, ErrorKind::Conflict);
    let missing = dir.path().join("nope.ncraft-plugin");
    assert_eq!(s.dispatch("plugin.import", &json!({"location": missing.to_str().unwrap()})).unwrap_err().kind, ErrorKind::NotFound);
    let junk = dir.path().join("junk.ncraft-plugin");
    std::fs::write(&junk, b"not a package").unwrap();
    assert_eq!(s.dispatch("plugin.import", &json!({"location": junk.to_str().unwrap()})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("plugin.enable", &json!({"package_id": "Not-An-Id"})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("plugin.disable", &json!({"package_id": "io.example.absent"})).unwrap_err().kind, ErrorKind::NotFound);
    assert!(s.dispatch("plugin.list", &json!({"extra": 1})).is_err());
    // A damaged trust file is reported, not treated as empty.
    let trust = dir.path().join("trust.json");
    std::fs::write(&trust, b"{oops").unwrap();
    let mut s = session(BTreeSet::new(), Some(trust));
    assert!(s.dispatch("plugin.import", &json!({"location": package(dir.path(), "io.example.other", "1.0.0")})).unwrap_err().message.contains("trust file"));
}

#[test]
fn without_the_service_the_commands_are_unsupported() {
    let mut s = Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap();
    assert_eq!(s.dispatch("plugin.list", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
}
