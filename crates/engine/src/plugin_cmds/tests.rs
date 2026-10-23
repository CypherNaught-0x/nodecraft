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

fn session_only() -> (Session, PickedPackage) {
    let mut services = Services::default();
    let store = Store::open(MemoryBackend::default()).unwrap();
    let picked = register_session_plugins(&mut services, store, PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() }).unwrap();
    let session = Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "P").unwrap()).unwrap().with_services(services);
    (session, picked)
}

/// The browser's store (#823): a picked file is imported by the location it was given, and lists
/// say the packages last only for the session.
#[test]
fn a_picked_package_is_imported_from_its_bytes_and_listed_as_session_only() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, picked) = session_only();
    let bytes = std::fs::read(package(dir.path(), "io.example.palette", "1.0.0")).unwrap();
    let location = picked.put("palette.ncraft-plugin", bytes).unwrap();
    let dry = s.dispatch("plugins.import", &json!({"location": location, "dry_run": true})).unwrap();
    let report = s.dispatch("plugins.import", &json!({"location": location, "digest": dry["digest"]})).unwrap();
    assert_eq!((dry["session_only"].clone(), report["session_only"].clone()), (json!(true), json!(true)));
    let released = s.dispatch("plugins.import", &json!({"location": location})).unwrap_err();
    assert_eq!(released.kind, ErrorKind::NotFound, "an installed pick is let go");
    let list = s.dispatch("plugins.list", &json!({})).unwrap();
    assert_eq!((list["session_only"].clone(), list["packages"][0]["package_id"].clone()), (json!(true), json!("io.example.palette")));
    s.dispatch("plugins.enable", &json!({"package_id": "io.example.palette"})).unwrap();
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["state"], "enabled", "a data package runs nothing, so it may be enabled");
    // Only the last pick is held; an earlier location, or a path, names nothing.
    let next = picked.put("other.ncraft-plugin", b"x".to_vec()).unwrap();
    assert_ne!(next, location);
    let gone = s.dispatch("plugins.import", &json!({"location": location})).unwrap_err();
    assert!((gone.kind == ErrorKind::NotFound) && gone.message.contains("choose it again"), "{gone:?}");
    let path = s.dispatch("plugins.import", &json!({"location": "/etc/hosts"})).unwrap_err();
    assert!(path.kind == ErrorKind::Unsupported && path.message.contains("can't read files by path"), "{path:?}");
    // An update comes from a pick too, says it is session-only, and lets the pick go.
    s.dispatch("plugins.disable", &json!({"package_id": "io.example.palette"})).unwrap();
    let v2 = picked.put("palette-2.ncraft-plugin", std::fs::read(package(dir.path(), "io.example.palette", "2.0.0")).unwrap()).unwrap();
    let updated = s.dispatch("plugins.update", &json!({"location": v2})).unwrap();
    assert_eq!((updated["version"].clone(), updated["session_only"].clone()), (json!("2.0.0"), json!(true)));
    assert_eq!(s.dispatch("plugins.update", &json!({"location": v2})).unwrap_err().kind, ErrorKind::NotFound, "an installed update is let go");
}

#[test]
fn a_picked_name_drops_marks_that_would_disguise_it() {
    let picked = PickedPackage::default();
    assert_eq!(picked.put("palette\u{202E}nigulp\u{200B}.ncraft-plugin", Vec::new()).unwrap(), "picked/1/palettenigulp.ncraft-plugin");
    assert_eq!(picked.put("\u{2066}/\u{0007}", Vec::new()).unwrap(), "picked/2/package");
}

/// Nothing runs in the browser: a package that would run can be installed, never granted or enabled.
#[test]
fn a_session_only_store_never_lets_a_package_run() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, picked) = session_only();
    let location = picked.put("shout.ncraft-plugin", std::fs::read(text_package(dir.path(), "io.example.shout", "1.0.0", SHOUT)).unwrap()).unwrap();
    s.dispatch("plugins.import", &json!({"location": location})).unwrap();
    for (id, params) in [("plugins.grant", json!({"package_id": "io.example.shout"})), ("plugins.enable", json!({"package_id": "io.example.shout"}))] {
        let error = s.dispatch(id, &params).unwrap_err();
        assert!(error.kind == ErrorKind::Unsupported && error.message.contains("can't run in the browser"), "{id}: {error:?}");
    }
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["state"], "disabled");
    assert!(s.plugins().kinds().is_empty());
}

#[test]
fn picked_files_are_bounded_and_their_names_kept_safe() {
    let picked = PickedPackage::default();
    let over = usize::try_from(package_limit()).unwrap() + 1;
    assert_eq!(picked.put("big", vec![0; over]).unwrap_err().kind, ErrorKind::InvalidInput);
    let location = picked.put(&format!("a/b\u{7}{}", "é".repeat(400)), Vec::new()).unwrap();
    assert!(location.starts_with("picked/") && location.len() <= 300 && location.matches('/').count() == 2, "{location}");
    assert!(picked.put("", Vec::new()).unwrap().ends_with("/package"));
}

/// `plugins.reload` (#866): the file a package was imported from, installed again as an update.
#[test]
fn reload_installs_the_remembered_file_again_and_keeps_a_data_package_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let pkg = json!({"package_id": "io.example.palette"});
    let file = dir.path().join("palette.ncraft-plugin");
    std::fs::copy(package(dir.path(), "io.example.palette", "1.0.0"), &file).unwrap();
    let location = file.to_str().unwrap();
    // A dry run remembers nothing; there is nothing installed to reload.
    s.dispatch("plugins.import", &json!({"location": location, "dry_run": true})).unwrap();
    assert_eq!(s.dispatch("plugins.reload", &pkg).unwrap_err().kind, ErrorKind::NotFound);
    s.dispatch("plugins.import", &json!({"location": location})).unwrap();
    let listed = s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0].clone();
    assert_eq!(
        (listed["reloadable"].clone(), listed["reload_file"].clone()),
        (json!(true), json!("palette.ncraft-plugin")),
        "the file's name, never its folders"
    );
    assert!(!listed.to_string().contains(dir.path().to_str().unwrap()));
    s.dispatch("plugins.enable", &pkg).unwrap();
    let before = s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0].clone();

    // Unchanged: refused, nothing changes.
    let unchanged = s.dispatch("plugins.reload", &pkg).unwrap_err();
    assert!(unchanged.kind == ErrorKind::Conflict && unchanged.message.contains("unchanged"), "{unchanged:?}");
    // Another package in the file now: refused, nothing changes.
    std::fs::copy(package(dir.path(), "io.example.other", "1.0.0"), &file).unwrap();
    let other = s.dispatch("plugins.reload", &pkg).unwrap_err();
    assert!(other.kind == ErrorKind::Conflict && other.message.contains("io.example.other"), "{other:?}");
    // A broken or missing file: refused, and the package stays enabled as it was.
    std::fs::write(&file, b"not a package").unwrap();
    assert_eq!(s.dispatch("plugins.reload", &pkg).unwrap_err().kind, ErrorKind::InvalidInput);
    std::fs::remove_file(&file).unwrap();
    assert_eq!(s.dispatch("plugins.reload", &pkg).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0], before);

    // A new version at the same path: updated, the old one kept, enabled again.
    std::fs::copy(package(dir.path(), "io.example.palette", "1.1.0"), &file).unwrap();
    let reloaded = s.dispatch("plugins.reload", &pkg).unwrap();
    assert_eq!((reloaded["version"].clone(), reloaded["replaced"].clone()), (json!("1.1.0"), before["digest"].clone()));
    assert_eq!((reloaded["reloaded_from"].clone(), reloaded["enabled"].clone(), reloaded["needs_grant"].clone()), (json!(location), json!(true), json!(false)));
    assert!(reloaded.get("enable_error").is_none());
    let after = s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0].clone();
    assert_eq!((after["state"].clone(), after["kept"].clone(), after["reloadable"].clone()), (json!("enabled"), json!([before["digest"]]), json!(true)));
    assert_eq!((reloaded["signer_changed"].clone(), reloaded["signed_by_before"].clone()), (json!(false), Value::Null));

    // Uninstalling forgets the file: installed again without a location, there is nothing to reload.
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let id = nodecraft_plugin::id::PackageId::parse("io.example.palette").unwrap();
    handle(&mut store, &host, &json!({"op": "import", "location": location})).unwrap();
    assert_eq!(store.source(&id).unwrap().as_deref(), Some(location));
    handle(&mut store, &host, &json!({"op": "uninstall", "package_id": "io.example.palette"})).unwrap();
    store.import(&std::fs::read(location).unwrap(), &TrustStore::default(), false).unwrap();
    let row = handle(&mut store, &host, &json!({"op": "list"})).unwrap()["packages"][0].clone();
    assert_eq!((row["reloadable"].clone(), row["reload_file"].clone()), (json!(false), Value::Null), "{row}");
    assert_eq!(handle(&mut store, &host, &json!({"op": "reload", "package_id": "io.example.palette"})).unwrap_err().kind, ErrorKind::NotFound);
}

/// A data package signed by another key (or newly signed, or no longer signed) is installed but
/// not enabled again: the person sees the new signer first. The same signer is enabled again.
#[test]
fn reload_leaves_a_data_package_disabled_when_its_signer_changed() {
    use nodecraft_plugin::signature::SigningKey;
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let pkg = json!({"package_id": "io.example.palette"});
    let file = dir.path().join("palette.ncraft-plugin");
    let location = file.to_str().unwrap();
    let key = |name: &str, seed: u8| SigningKey::from_secret(nodecraft_plugin::id::Token::parse(name).unwrap(), [seed; 32]);
    let write = |version: &str, signer: Option<&SigningKey>| {
        let files = vec![
            ("manifest.json".to_owned(), manifest("io.example.palette", version).into_bytes()),
            ("LICENSE".to_owned(), b"MIT\n".to_vec()),
            ("d/warm.json".to_owned(), b"{}".to_vec()),
        ];
        let packed = match signer {
            Some(k) => nodecraft_plugin_store::author::pack_signed(files, k).unwrap(),
            None => nodecraft_plugin_store::author::pack(files, None).unwrap(),
        };
        std::fs::write(&file, packed.bytes).unwrap();
    };
    let (alice, mallory) = (key("alice-key", 1), key("mallory-key", 2));
    write("1.0.0", Some(&alice));
    s.dispatch("plugins.import", &json!({"location": location})).unwrap();
    s.dispatch("plugins.enable", &pkg).unwrap();

    // The same signer: enabled again.
    write("1.1.0", Some(&alice));
    let same = s.dispatch("plugins.reload", &pkg).unwrap();
    assert_eq!((same["enabled"].clone(), same["signer_changed"].clone()), (json!(true), json!(false)), "{same}");

    // Another key, then none: each installed, left disabled, and the result says so.
    for (version, signer, before) in [("1.2.0", Some(&mallory), "alice-key"), ("1.3.0", None, "mallory-key")] {
        write(version, signer);
        let changed = s.dispatch("plugins.reload", &pkg).unwrap();
        assert_eq!(
            (changed["version"].clone(), changed["enabled"].clone(), changed["signer_changed"].clone(), changed["signed_by_before"].clone()),
            (json!(version), json!(false), json!(true), json!(before)),
            "{changed}"
        );
        assert_eq!(s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0]["state"], "disabled");
        s.dispatch("plugins.enable", &pkg).unwrap();
    }
}

/// A store whose Nth index write fails: the update's own write, after the package was disabled
/// and its grant revoked.
struct FailingIndex {
    inner: MemoryBackend,
    writes_left: std::cell::Cell<Option<u32>>,
}

impl nodecraft_plugin_store::StoreBackend for FailingIndex {
    fn read_index(&self) -> Result<Option<Vec<u8>>, Error> {
        self.inner.read_index()
    }
    fn write_index(&mut self, bytes: &[u8]) -> Result<(), Error> {
        match self.writes_left.get() {
            Some(0) => {
                self.writes_left.set(None);
                Err(Error::new(ErrorKind::Internal, "the plugin store couldn't be written (disk full)"))
            }
            Some(n) => {
                self.writes_left.set(Some(n - 1));
                self.inner.write_index(bytes)
            }
            None => self.inner.write_index(bytes),
        }
    }
    fn read_object(&self, digest: &nodecraft_plugin::PackageDigest) -> Result<Vec<u8>, Error> {
        self.inner.read_object(digest)
    }
    fn write_object(&mut self, digest: &nodecraft_plugin::PackageDigest, bytes: &[u8]) -> Result<(), Error> {
        self.inner.write_object(digest, bytes)
    }
    fn remove_object(&mut self, digest: &nodecraft_plugin::PackageDigest) -> Result<(), Error> {
        self.inner.remove_object(digest)
    }
    fn list_objects(&self) -> Result<Vec<nodecraft_plugin::PackageDigest>, Error> {
        self.inner.list_objects()
    }
    fn read_grants(&self) -> Result<Option<Vec<u8>>, Error> {
        self.inner.read_grants()
    }
    fn write_grants(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.inner.write_grants(bytes)
    }
    fn read_sources(&self) -> Result<Option<Vec<u8>>, Error> {
        self.inner.read_sources()
    }
    fn write_sources(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.inner.write_sources(bytes)
    }
}

/// If the update fails after its grant was revoked, the package can't be enabled again, and the
/// error says it was left disabled rather than suggesting nothing changed.
#[test]
fn a_failed_reload_says_when_it_left_the_package_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("text.ncraft-plugin");
    let location = file.to_str().unwrap();
    std::fs::copy(text_package(dir.path(), "io.example.text", "1.0.0", SHOUT), &file).unwrap();
    let mut store = Store::open(FailingIndex { inner: MemoryBackend::default(), writes_left: std::cell::Cell::new(None) }).unwrap();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let pkg = json!({"package_id": "io.example.text"});
    handle(&mut store, &host, &json!({"op": "import", "location": location})).unwrap();
    handle(&mut store, &host, &json!({"op": "grant", "package_id": "io.example.text"})).unwrap();
    handle(&mut store, &host, &json!({"op": "enable", "package_id": "io.example.text"})).unwrap();
    std::fs::copy(text_package(dir.path(), "io.example.text", "1.1.0", SHOUT), &file).unwrap();
    // The disable writes the index; the update's write fails.
    store.backend().writes_left.set(Some(1));
    let mut request = pkg.clone();
    request["op"] = json!("reload");
    let error = handle(&mut store, &host, &request).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.contains("disk full") && error.message.contains("left disabled") && error.message.contains("grant it again"), "{error:?}");
    let row = handle(&mut store, &host, &json!({"op": "list"})).unwrap()["packages"][0].clone();
    assert_eq!((row["version"].clone(), row["state"].clone()), (json!("1.0.0"), json!("disabled")), "{row}");
}

/// A location that can't be saved (a sources file from a newer NodeCraft) doesn't stop the
/// import, and isn't written over; the result says Reload won't know the file.
#[test]
fn an_import_whose_location_cant_be_saved_still_installs_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let newer = br#"{"sources":{},"sources_version":2}"#.to_vec();
    let mut store = Store::open(MemoryBackend { sources: Some(newer.clone()), ..MemoryBackend::default() }).unwrap();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let out = handle(&mut store, &host, &json!({"op": "import", "location": package(dir.path(), "io.example.palette", "1.0.0")})).unwrap();
    assert!(out["location_not_saved"].as_str().is_some_and(|m| m.contains("newer NodeCraft")), "{out}");
    assert_eq!(store.backend().sources.as_deref(), Some(&newer[..]));
    assert_eq!(handle(&mut store, &host, &json!({"op": "list"})).unwrap()["packages"][0]["reloadable"], false);
}

#[test]
fn reload_never_carries_a_grant_over_and_needs_a_path() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session(BTreeSet::new(), None);
    let pkg = json!({"package_id": "io.example.text"});
    let file = dir.path().join("text.ncraft-plugin");
    std::fs::copy(text_package(dir.path(), "io.example.text", "1.0.0", SHOUT), &file).unwrap();
    s.dispatch("plugins.import", &json!({"location": file.to_str().unwrap()})).unwrap();
    s.dispatch("plugins.grant", &pkg).unwrap();
    s.dispatch("plugins.enable", &pkg).unwrap();
    std::fs::copy(text_package(dir.path(), "io.example.text", "1.1.0", SHOUT), &file).unwrap();
    let reloaded = s.dispatch("plugins.reload", &pkg).unwrap();
    assert_eq!((reloaded["version"].clone(), reloaded["enabled"].clone(), reloaded["needs_grant"].clone()), (json!("1.1.0"), json!(false), json!(true)));
    let row = s.dispatch("plugins.list", &json!({})).unwrap()["packages"][0].clone();
    assert_eq!((row["state"].clone(), row["grant"].clone()), (json!("disabled"), Value::Null), "the person grants the new version");
    // Granting and enabling the new version works as for any update.
    s.dispatch("plugins.grant", &pkg).unwrap();
    s.dispatch("plugins.enable", &pkg).unwrap();

    // Without a remembered file (installed some other way), and with bad params.
    let mut store = Store::open(MemoryBackend::default()).unwrap();
    store.import(&std::fs::read(package(dir.path(), "io.example.palette", "1.0.0")).unwrap(), &TrustStore::default(), false).unwrap();
    let host = PluginHost { trust: None, taken: BTreeSet::new(), bindings: BTreeSet::new() };
    let unknown = handle(&mut store, &host, &json!({"op": "reload", "package_id": "io.example.palette"})).unwrap_err();
    assert!(unknown.kind == ErrorKind::NotFound && unknown.message.contains("which file"), "{unknown:?}");
    assert_eq!(handle(&mut store, &host, &json!({"op": "reload", "package_id": "io.example.none"})).unwrap_err().kind, ErrorKind::NotFound);
    for bad in [json!({}), json!({"package_id": "Bad"}), json!({"package_id": "io.example.text", "location": "/x"})] {
        assert_eq!(s.dispatch("plugins.reload", &bad).unwrap_err().kind, ErrorKind::InvalidInput, "{bad}");
    }
}

#[test]
fn the_browser_store_refuses_reload() {
    let (mut s, _picked) = session_only();
    let error = s.dispatch("plugins.reload", &json!({"package_id": "io.example.palette"})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
}
