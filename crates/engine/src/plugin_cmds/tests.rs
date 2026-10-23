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
    let dry = s.dispatch("plugins.import", &json!({"location": v1, "dry_run": true})).unwrap();
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["provenance"], "unsigned");
    assert_eq!(dry["registrations"], json!(["plugin.io.example.palette.warm"]));
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap(), json!({"packages": []}), "a dry run installs nothing");
    s.dispatch("plugins.import", &json!({"location": v1})).unwrap();
    let list = s.dispatch("plugins.list", &json!({})).unwrap();
    assert_eq!(list["packages"][0]["state"], "disabled");
    let enabled = s.dispatch("plugins.enable", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(enabled, json!({"registered": ["plugin.io.example.palette.warm"], "unbound": [], "grant": null}));
    assert_eq!(
        s.dispatch("plugins.update", &json!({"location": package(dir.path(), "io.example.palette", "1.1.0")})).unwrap_err().kind,
        ErrorKind::Conflict,
        "disable first"
    );
    s.dispatch("plugins.disable", &json!({"package_id": "io.example.palette"})).unwrap();
    let updated = s.dispatch("plugins.update", &json!({"location": package(dir.path(), "io.example.palette", "1.1.0")})).unwrap();
    let old = updated["replaced"].as_str().unwrap().to_owned();
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["kept"], json!([old.clone()]));
    s.dispatch("plugins.rollback", &json!({"package_id": "io.example.palette", "digest": old})).unwrap();
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["version"], "1.0.0");
    let gone = s.dispatch("plugins.uninstall", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(gone["removed"].as_array().unwrap().len(), 2);
    assert_eq!((s.document().clone(), s.revision()), before, "no document change");
    assert!(!s.can_undo(), "no undo step");
}

#[test]
fn bad_requests_are_errors_the_person_can_act_on() {
    let dir = tempfile::tempdir().unwrap();
    // A host ID a package would take.
    let mut s = session(BTreeSet::from(["plugin.io.example.palette.warm".to_owned()]), None);
    s.dispatch("plugins.import", &json!({"location": package(dir.path(), "io.example.palette", "1.0.0")})).unwrap();
    assert_eq!(s.dispatch("plugins.enable", &json!({"package_id": "io.example.palette"})).unwrap_err().kind, ErrorKind::Conflict);
    let missing = dir.path().join("nope.ncraft-plugin");
    assert_eq!(s.dispatch("plugins.import", &json!({"location": missing.to_str().unwrap()})).unwrap_err().kind, ErrorKind::NotFound);
    let junk = dir.path().join("junk.ncraft-plugin");
    std::fs::write(&junk, b"not a package").unwrap();
    assert_eq!(s.dispatch("plugins.import", &json!({"location": junk.to_str().unwrap()})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("plugins.enable", &json!({"package_id": "Not-An-Id"})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("plugins.disable", &json!({"package_id": "io.example.absent"})).unwrap_err().kind, ErrorKind::NotFound);
    assert!(s.dispatch("plugins.list", &json!({"extra": 1})).is_err());
    // A damaged trust file is reported, not treated as empty.
    let trust = dir.path().join("trust.json");
    std::fs::write(&trust, b"{oops").unwrap();
    let mut s = session(BTreeSet::new(), Some(trust));
    assert!(s.dispatch("plugins.import", &json!({"location": package(dir.path(), "io.example.other", "1.0.0")})).unwrap_err().message.contains("trust file"));
}

#[test]
fn without_the_service_the_commands_are_unsupported() {
    let mut s = Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap();
    assert_eq!(s.enabled("plugins.list", &json!({})).unwrap_err().kind, ErrorKind::Unsupported, "shown as unavailable");
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn locations_must_be_absolute_files_and_unknown_signers_are_named() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    assert!(s.dispatch("plugins.import", &json!({"location": "relative.ncraft-plugin"})).unwrap_err().message.contains("absolute"));
    assert_eq!(s.dispatch("plugins.import", &json!({"location": dir.path().to_str().unwrap()})).unwrap_err().kind, ErrorKind::InvalidInput, "a folder");
    // Another window changing the store is seen by the next request.
    let shared = tempfile::tempdir().unwrap();
    let open = || {
        let mut services = Services::default();
        let store = Store::open(nodecraft_plugin_store::FolderBackend::new(shared.path().to_path_buf()).unwrap()).unwrap();
        register_plugin_service(&mut services, store, PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() }).unwrap();
        Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap().with_services(services)
    };
    let (mut a, mut b) = (open(), open());
    a.dispatch("plugins.import", &json!({"location": package(dir.path(), "io.example.palette", "1.0.0")})).unwrap();
    b.dispatch("plugins.enable", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(a.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["state"], "enabled", "not a stale view");
    a.dispatch("plugins.disable", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(b.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["state"], "disabled");
}

/// Writes a text action package whose one action is `definition`.
fn text_package(dir: &std::path::Path, id: &str, version: &str, definition: &str) -> String {
    let manifest =
        manifest(id, version).replace(r#"{"kind":"data"}"#, r#"{"kind":"text_actions","schema":1}"#).replace(r#""kind":"catalog""#, r#""kind":"action""#);
    let bytes = archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/warm.json", definition.as_bytes())], None).unwrap();
    let path = dir.join(format!("{id}-{version}.ncraft-plugin"));
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

const SHOUT: &str = r#"{"id":"user.shout","inputs":[{"kind":"text","name":"words"}],"label":"Shout","outputs":[{"expr":"upper(words)","kind":"text","name":"out"}],"schema":1,"version":1}"#;

#[test]
fn a_text_action_package_runs_only_once_granted() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let pkg = json!({"package_id": "io.example.text"});
    s.dispatch("plugins.import", &json!({"location": text_package(dir.path(), "io.example.text", "1.0.0", SHOUT)})).unwrap();
    assert_eq!(s.dispatch("plugins.enable", &pkg).unwrap_err().kind, ErrorKind::Forbidden);
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["grant"], Value::Null);
    let grant = s.dispatch("plugins.grant", &pkg).unwrap();
    assert_eq!((grant["grant_id"].as_str(), grant["epoch"].as_u64(), grant["package_id"].as_str()), (Some("g1"), Some(1), Some("io.example.text")));
    assert_eq!(
        s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["grant"],
        json!({"grant_id": "g1", "epoch": 1, "capabilities": [], "origins": []})
    );
    let enabled = s.dispatch("plugins.enable", &pkg).unwrap();
    assert_eq!(enabled["registered"], json!(["plugin.io.example.text.warm"]));
    assert_eq!(enabled["grant"], json!({"grant_id": "g1", "epoch": 1, "capabilities": [], "origins": []}));
    let revoked = s.dispatch("plugins.revoke", &json!({"grant_id": "g1"})).unwrap();
    assert_eq!(revoked, json!({"grant_id": "g1", "revoked": true, "disabled": true, "dependents": []}));
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["state"], "disabled");
    for bad in ["g0", "g", "x1", "g01", "g123456789012345678901"] {
        assert_eq!(s.dispatch("plugins.revoke", &json!({"grant_id": bad})).unwrap_err().kind, ErrorKind::InvalidInput, "{bad}");
    }
    assert_eq!(s.dispatch("plugins.revoke", &json!({"grant_id": "g9"})).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn a_broken_definition_is_refused_before_it_is_granted() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let two = SHOUT.replace(
        r#"{"expr":"upper(words)","kind":"text","name":"out"}"#,
        r#"{"expr":"upper(words)","kind":"text","name":"out"},{"expr":"lower(words)","kind":"text","name":"low"}"#,
    );
    for (i, definition) in
        [SHOUT.replace("upper(words)", "upper(nope)"), SHOUT.replace(r#""schema":1"#, r#""schema":2"#), "[]".to_owned(), two].into_iter().enumerate()
    {
        let id = format!("io.example.bad{i}");
        s.dispatch("plugins.import", &json!({"location": text_package(dir.path(), &id, "1.0.0", &definition)})).unwrap();
        let error = s.dispatch("plugins.grant", &json!({"package_id": id})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{definition}");
        assert!(error.message.contains("its action warm can't be used"), "{}", error.message);
        assert_eq!(s.dispatch("plugins.enable", &json!({"package_id": id})).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    // A data package needs no grant.
    s.dispatch("plugins.import", &json!({"location": package(dir.path(), "io.example.palette", "1.0.0")})).unwrap();
    assert_eq!(s.dispatch("plugins.grant", &json!({"package_id": "io.example.palette"})).unwrap_err().kind, ErrorKind::InvalidInput);
}

/// A provider operations package (#174) whose one provider's template reaches `cdn`.
fn provider_package(dir: &std::path::Path, version: &str, cdn: &str) -> String {
    provider_package_asking(dir, version, cdn, r#""optional":[],"required":["network","secret"]"#)
}

/// `provider_package` whose manifest asks for `capabilities`.
fn provider_package_asking(dir: &std::path::Path, version: &str, cdn: &str, capabilities: &str) -> String {
    let manifest = manifest("io.example.acme", version)
        .replace(r#"{"kind":"data"}"#, r#"{"kind":"provider_operations","schema":1}"#)
        .replace(r#""kind":"catalog""#, r#""kind":"provider""#)
        .replace(r#""optional":[],"required":[]"#, capabilities);
    assert!(manifest.contains(capabilities), "{manifest}");
    let template = json!({
        "schema": 1, "label": "Acme", "origin": "https://api.acme.example", "output_origins": [cdn], "credential": "bearer",
        "models": [{"id": "acme/v1", "price": {"currency": "USD", "micros": "40000"}}],
        "submit": {"method": "POST", "path": "/v1/{model}", "body": {"prompt": "{text:prompt}"}, "outputs": [{"at": "/url", "kind": "url", "mime": "image/png"}]},
        "limits": {"response_bytes": 65536, "timeout_ms": 30000, "concurrency": 1}
    })
    .to_string();
    let bytes = archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/warm.json", template.as_bytes())], None).unwrap();
    let path = dir.join(format!("io.example.acme-{version}.ncraft-plugin"));
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

#[test]
fn a_provider_package_is_granted_exactly_the_rights_it_showed() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let imported = s.dispatch("plugins.import", &json!({"location": provider_package(dir.path(), "1.0.0", "https://cdn.acme.example")})).unwrap();
    let origins = json!(["https://api.acme.example", "https://cdn.acme.example"]);
    assert_eq!(imported["rights"], json!({"required": ["network", "secret"], "optional": [], "origins": origins}));
    let shown = s.dispatch("plugins.rights", &json!({"package_id": "io.example.acme"})).unwrap();
    assert_eq!((&shown["origins"], &shown["grant"]), (&origins, &Value::Null));
    let digest = shown["digest"].clone();
    let both = json!(["network", "secret"]);
    let grant = |s: &mut Session, p: Value| s.dispatch("plugins.grant", &p);
    // A bare grant, a stale digest, a missing capability or other origins grant nothing.
    assert_eq!(grant(&mut s, json!({"package_id": "io.example.acme"})).unwrap_err().kind, ErrorKind::InvalidInput);
    let stale = format!("blake3:{}", "0".repeat(64));
    assert_eq!(
        grant(&mut s, json!({"package_id": "io.example.acme", "digest": stale, "capabilities": both, "origins": origins})).unwrap_err().kind,
        ErrorKind::Conflict
    );
    assert_eq!(
        grant(&mut s, json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["network"], "origins": origins})).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    let fewer = json!(["https://api.acme.example"]);
    assert_eq!(
        grant(&mut s, json!({"package_id": "io.example.acme", "digest": digest, "capabilities": both, "origins": fewer})).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(
        grant(&mut s, json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["network", "secret", "process"], "origins": origins}))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidInput
    );
    assert_eq!(s.dispatch("plugins.rights", &json!({"package_id": "io.example.acme"})).unwrap()["grant"], Value::Null);
    let granted = grant(&mut s, json!({"package_id": "io.example.acme", "digest": digest, "capabilities": both, "origins": origins})).unwrap();
    assert_eq!((&granted["capabilities"], &granted["origins"]), (&both, &origins));
    assert_eq!(s.dispatch("plugins.rights", &json!({"package_id": "io.example.acme"})).unwrap()["grant"]["grant_id"], granted["grant_id"]);
    // An update that reaches a new origin shows it and drops the grant.
    let updated = s.dispatch("plugins.update", &json!({"location": provider_package(dir.path(), "1.1.0", "https://files.acme.example")})).unwrap();
    assert_eq!(
        updated["rights_delta"],
        json!({"added_capabilities": [], "removed_capabilities": [], "added_origins": ["https://files.acme.example"], "removed_origins": ["https://cdn.acme.example"]})
    );
    assert_eq!(s.dispatch("plugins.rights", &json!({"package_id": "io.example.acme"})).unwrap()["grant"], Value::Null);
}

#[test]
fn grant_requests_are_bounded_before_they_are_read() {
    let mut s = session(BTreeSet::new(), None);
    let many: Vec<String> = (0..9).map(|i| format!("https://o{i}.example")).collect();
    for bad in [
        json!({"package_id": "io.example.acme", "origins": many}),
        json!({"package_id": "io.example.acme", "capabilities": ["x".repeat(33)]}),
        json!({"package_id": "io.example.acme", "capabilities": [1]}),
        json!({"package_id": "io.example.acme", "digest": "sha256:00"}),
        json!({"package_id": "io.example.acme", "digest": 7}),
        json!({"package_id": "io.example.acme", "extra": true}),
    ] {
        assert_eq!(s.dispatch("plugins.grant", &bad).unwrap_err().kind, ErrorKind::InvalidInput, "{bad}");
    }
}

#[test]
fn an_optional_capability_is_granted_only_as_named() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let path = provider_package_asking(dir.path(), "1.0.0", "https://cdn.acme.example", r#""optional":["secret"],"required":["network"]"#);
    s.dispatch("plugins.import", &json!({"location": path})).unwrap();
    let shown = s.dispatch("plugins.rights", &json!({"package_id": "io.example.acme"})).unwrap();
    assert_eq!((&shown["required"], &shown["optional"]), (&json!(["network"]), &json!(["secret"])));
    let digest = shown["digest"].clone();
    let origins = json!(["https://api.acme.example", "https://cdn.acme.example"]);
    let grant = |s: &mut Session, p: Value| s.dispatch("plugins.grant", &p);
    // No digest, a missing required capability, or a name twice is refused, not narrowed.
    for bad in [
        json!({"package_id": "io.example.acme", "origins": origins}),
        json!({"package_id": "io.example.acme", "digest": digest}),
        json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["secret"], "origins": origins}),
        json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["network", "network"], "origins": origins}),
        json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["network"], "origins": [origins[0], origins[0], origins[1]]}),
    ] {
        assert_eq!(grant(&mut s, bad.clone()).unwrap_err().kind, ErrorKind::InvalidInput, "{bad}");
    }
    let network = grant(&mut s, json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["network"], "origins": origins})).unwrap();
    assert_eq!((&network["capabilities"], &network["origins"]), (&json!(["network"]), &origins));
    // Granting the optional one too replaces the live grant under a new epoch; the same rights again keep it.
    let both = json!({"package_id": "io.example.acme", "digest": digest, "capabilities": ["network", "secret"], "origins": origins});
    let more = grant(&mut s, both.clone()).unwrap();
    assert!(more["epoch"].as_u64() > network["epoch"].as_u64());
    assert_eq!(grant(&mut s, both).unwrap()["grant_id"], more["grant_id"]);
    assert_eq!(s.dispatch("plugins.rights", &json!({"package_id": "io.example.acme"})).unwrap()["grant"]["grant_id"], more["grant_id"]);
}

/// The person reads the licence before installing (#172): its text comes with the dry run, cut
/// at a character boundary when long.
#[test]
fn a_dry_run_carries_the_notice_text_cut_at_a_character_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = manifest("io.example.palette", "1.0.0");
    let long = "é".repeat(MAX_NOTICE_BYTES);
    let files = [("manifest.json", manifest.as_bytes()), ("LICENSE", long.as_bytes()), ("NOTICE", b"Made by Example.\n".as_slice()), ("d/warm.json", b"{}")];
    let path = dir.path().join("p.ncraft-plugin");
    std::fs::write(&path, archive::build(&files, None).unwrap()).unwrap();
    let mut s = session(BTreeSet::new(), None);
    let dry = s.dispatch("plugins.import", &json!({"location": path.to_str().unwrap(), "dry_run": true})).unwrap();
    let texts = dry["notice_texts"].as_array().unwrap();
    assert_eq!(texts.len(), 2);
    let license = texts.iter().find(|t| t["path"] == "LICENSE").unwrap();
    let text = license["text"].as_str().unwrap();
    assert!(text.len() <= MAX_NOTICE_BYTES && text.len() > MAX_NOTICE_BYTES - 2, "{}", text.len());
    assert!(text.chars().all(|c| c == 'é'));
    assert_eq!(license["truncated"], true);
    let notice = texts.iter().find(|t| t["path"] == "NOTICE").unwrap();
    assert_eq!((notice["text"].as_str(), &notice["truncated"]), (Some("Made by Example.\n"), &json!(false)));
    // A data package runs nothing: its rights say so.
    s.dispatch("plugins.import", &json!({"location": path.to_str().unwrap()})).unwrap();
    assert_eq!(s.dispatch("plugins.rights", &json!({"package_id": "io.example.palette"})).unwrap()["runs"], false);
}

#[test]
fn an_install_names_the_digest_it_was_shown_and_a_changed_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = package(dir.path(), "io.example.palette", "1.0.0");
    let mut s = session(BTreeSet::new(), None);
    let shown = s.dispatch("plugins.import", &json!({"location": path, "dry_run": true})).unwrap()["digest"].as_str().unwrap().to_owned();
    // The file is replaced by another version after the person looked at it.
    let other = package(dir.path(), "io.example.palette", "2.0.0");
    std::fs::rename(&other, &path).unwrap();
    let error = s.dispatch("plugins.import", &json!({"location": path, "digest": shown})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict, "{error:?}");
    assert!(error.message.contains("changed since it was checked"));
    assert!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"].as_array().unwrap().is_empty(), "nothing installed");
    for digest in [json!("blake3:zz"), json!(7), json!("x".repeat(100))] {
        let error = s.dispatch("plugins.import", &json!({"location": path, "digest": digest})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{digest}");
    }
    // The digest it has now installs.
    let now = s.dispatch("plugins.import", &json!({"location": path, "dry_run": true})).unwrap()["digest"].clone();
    s.dispatch("plugins.import", &json!({"location": path, "digest": now})).unwrap();
}
