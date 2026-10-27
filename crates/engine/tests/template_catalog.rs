//! Author CI calls the exact production validator on a supplied repository, never a provider.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::templates::{self, MAX_BYTES, MAX_INDEX_BYTES};
use std::{io::Read, path::Path};

fn invalid(message: &'static str) -> nodecraft_types::Error {
    nodecraft_types::Error::new(nodecraft_types::ErrorKind::InvalidInput, message)
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>, nodecraft_types::Error> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| invalid("catalog file is missing"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("catalog files must be ordinary files"));
    }
    let file = std::fs::File::open(path).map_err(|_| invalid("catalog file could not be opened"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|_| invalid("catalog file could not be read"))?;
    if bytes.len() > limit {
        return Err(invalid("catalog file exceeds its size limit"));
    }
    Ok(bytes)
}

#[test]
#[ignore = "author CI: TEMPLATE953_CATALOG_ROOT selects the external data repository"]
fn validate_external_catalog() {
    let root = std::path::PathBuf::from(std::env::var_os("TEMPLATE953_CATALOG_ROOT").unwrap()).canonicalize().unwrap();
    let baseline = std::env::var_os("TEMPLATE953_BASE_HISTORY").map(|path| read(Path::new(&path), templates::MAX_HISTORY_BYTES).unwrap());
    validate_directory(&root, baseline.as_deref()).unwrap();
}
fn inventory(dir: &Path, root: &Path, depth: usize, left: &mut usize, files: &mut std::collections::BTreeSet<String>) -> Result<(), nodecraft_types::Error> {
    if depth > 3 {
        return Err(invalid("catalog archive directory is too deep"));
    }
    for entry in std::fs::read_dir(dir).map_err(|_| invalid("catalog archive directory is missing"))? {
        *left = left.checked_sub(1).ok_or_else(|| invalid("catalog archive inventory is too large"))?;
        let entry = entry.map_err(|_| invalid("catalog archive directory could not be read"))?;
        let ty = entry.file_type().map_err(|_| invalid("catalog file type could not be read"))?;
        if ty.is_symlink() {
            return Err(invalid("catalog archive symlinks are not allowed"));
        }
        if ty.is_dir() {
            inventory(&entry.path(), root, depth + 1, left, files)?;
        } else {
            if !ty.is_file() || !entry.path().extension().is_some_and(|extension| extension == "ncraft") {
                return Err(invalid("unexpected catalog archive file"));
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| invalid("archive escaped the catalog root"))?
                .to_str()
                .ok_or_else(|| invalid("catalog archive path is not UTF-8"))?
                .replace('\\', "/");
            if !files.insert(relative) || files.len() > templates::MAX_HISTORY_ENTRIES {
                return Err(invalid("catalog archive inventory is too large"));
            }
        }
    }
    Ok(())
}
fn validate_directory(root: &Path, baseline: Option<&[u8]>) -> Result<usize, nodecraft_types::Error> {
    let entries = templates::parse_index(&read(&root.join("index.json"), MAX_INDEX_BYTES)?)?;
    let history = templates::parse_history(&read(&root.join("history.json"), templates::MAX_HISTORY_BYTES)?)?;
    if let Some(baseline) = baseline {
        for old in templates::parse_history(baseline)? {
            if !history.contains(&old) {
                return Err(invalid("published history ledger cannot remove or alter an older record"));
            }
        }
    }
    for current in &entries {
        let published = history
            .iter()
            .find(|old| old.id == current.id && old.version == current.version)
            .ok_or_else(|| invalid("current template is absent from the complete published history ledger"))?;
        if current.blake3 != published.blake3
            || current.file != published.file
            || current.canvas != published.canvas
            || history.iter().any(|old| old.id == current.id && old.version > current.version)
        {
            return Err(invalid("current template does not match its latest immutable published identity"));
        }
    }
    let expected = history.iter().map(|entry| entry.file.clone()).collect::<std::collections::BTreeSet<_>>();
    let mut files = std::collections::BTreeSet::new();
    let mut left = templates::MAX_HISTORY_ENTRIES * 4;
    inventory(&root.join("templates"), root, 0, &mut left, &mut files)?;
    if files != expected {
        return Err(invalid("archive inventory must exactly match complete published history"));
    }
    let (kinds, _) = nodecraft_engine::generation_catalog::default_registry()?;
    let mut canvas_ids = std::collections::BTreeSet::new();
    for entry in &history {
        let path = root.join(&entry.file);
        if !path.canonicalize().map_err(|_| invalid("published archive is missing"))?.starts_with(root) {
            return Err(invalid("archive escaped the catalog root"));
        }
        let snapshot = templates::validate_archive(entry, &read(&path, MAX_BYTES)?)?;
        templates::validate_nodes(&snapshot, &kinds)?;
        for id in snapshot.project.canvases().keys() {
            if !canvas_ids.insert(*id) {
                return Err(invalid("published versions reuse a historical canvas ID"));
            }
        }
    }
    Ok(entries.len())
}

#[test]
fn unsupported_schema_and_metadata_are_refused() {
    let original: serde_json::Value = serde_json::from_str(nodecraft_template_data::INDEX).unwrap();
    for schema in [serde_json::Value::Null, serde_json::json!(0), serde_json::json!(2), serde_json::json!("1")] {
        let mut value = original.clone();
        value["schema_version"] = schema;
        assert!(templates::parse_index(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = original.clone();
    value.as_object_mut().unwrap().remove("schema_version");
    assert!(templates::parse_index(&serde_json::to_vec(&value).unwrap()).is_err());
    for field in ["id", "file", "blake3", "title", "description"] {
        let mut value = original.clone();
        value["templates"][0][field] = serde_json::json!(if matches!(field, "title" | "description") { "" } else { "../../outside" });
        assert!(templates::parse_index(&serde_json::to_vec(&value).unwrap()).is_err(), "{field}");
    }
    assert!(templates::parse_index(&vec![b' '; MAX_INDEX_BYTES + 1]).is_err());
}

#[test]
fn changed_bytes_or_missing_root_canvas_are_refused() {
    let entry = templates::entries().unwrap().remove(0);
    let (_, bytes) = nodecraft_template_data::FILES.iter().find(|(name, _)| *name == entry.file).unwrap();
    let mut changed = bytes.to_vec();
    changed[0] ^= 1;
    assert!(templates::validate_archive(&entry, &changed).is_err());
    let mut wrong = entry.clone();
    wrong.canvas = nodecraft_types::CanvasId::from_bytes([254; 16]);
    assert!(templates::validate_archive(&wrong, bytes).is_err());
    assert!(templates::validate_archive(&entry, &vec![0; MAX_BYTES + 1]).is_err());
}

#[test]
fn positive_template_versions_are_supported_and_immutable_paths_are_checked() {
    let mut value: serde_json::Value = serde_json::from_str(nodecraft_template_data::INDEX).unwrap();
    value["templates"][0]["version"] = serde_json::json!(2);
    let entry = &mut value["templates"][0];
    entry["file"] = serde_json::json!(format!("templates/{}/2/{}.ncraft", entry["id"].as_str().unwrap(), entry["blake3"].as_str().unwrap()));
    assert!(templates::parse_index(&serde_json::to_vec(&value).unwrap()).is_ok());
    value["templates"][0]["version"] = serde_json::json!(0);
    assert!(templates::parse_index(&serde_json::to_vec(&value).unwrap()).is_err());
}

fn versioned(version: u32) -> (templates::Entry, Vec<u8>, Vec<u8>) {
    let mut entry = templates::entries().unwrap().into_iter().find(|entry| entry.id == "generate-text").unwrap();
    let (_, original) = nodecraft_template_data::FILES.iter().find(|(name, _)| *name == entry.file).unwrap();
    let bytes = if version == 1 {
        original.to_vec()
    } else {
        let snapshot = templates::load(&entry.id).unwrap();
        let old = snapshot.project.canvases().get(&entry.canvas).unwrap();
        let mut id = [224; 16];
        id[12..].copy_from_slice(&version.to_le_bytes());
        let canvas = nodecraft_types::CanvasId::from_bytes(id);
        let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([223; 16]), "New template version").unwrap();
        nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Example") }.apply(&mut project).unwrap();
        for node in old.nodes().values() {
            nodecraft_graph::Change::AddNode { canvas, node: node.clone() }.apply(&mut project).unwrap();
        }
        for edge in old.edges().values() {
            nodecraft_graph::Change::Connect { canvas, edge: edge.clone() }.apply(&mut project).unwrap();
        }
        entry.canvas = canvas;
        nodecraft_format::encode(&nodecraft_format::Snapshot { project, assets: snapshot.assets, epoch: 0 }).unwrap()
    };
    entry.version = version;
    entry.blake3 = blake3::hash(&bytes).to_hex().to_string();
    entry.file = format!("templates/{}/{version}/{}.ncraft", entry.id, entry.blake3);
    let index = serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[entry]})).unwrap();
    (entry, bytes, index)
}

#[test]
fn refresh_keeps_bundled_and_cached_versions_available_when_latest_is_missing_or_invalid() {
    let mut catalog = templates::catalog::Catalog::default();
    let (two, bytes, index) = versioned(2);
    catalog.install_index(&index).unwrap();
    assert!(catalog.resolve(&two.id, Some(2), None).is_err());
    assert!(catalog.resolve(&two.id, Some(1), None).is_ok());
    catalog.install_archive(two.clone(), bytes).unwrap();
    assert!(catalog.resolve(&two.id, Some(2), Some(&two.blake3)).is_ok());
    let (_, _, three) = versioned(3);
    catalog.install_index(&three).unwrap();
    assert!(catalog.resolve(&two.id, Some(2), None).is_ok());
    assert!(catalog.resolve(&two.id, Some(3), None).is_err());
    let before = serde_json::to_value(catalog.listing().unwrap()).unwrap();
    assert!(catalog.install_index(b"invalid").is_err());
    assert_eq!(serde_json::to_value(catalog.listing().unwrap()).unwrap(), before);
    assert!(catalog.resolve(&two.id, None, None).is_ok());
}
#[test]
fn cache_eviction_bounds_retained_versions_and_keeps_the_original_bundle() {
    let mut catalog = templates::catalog::Catalog::default();
    for version in 2..150 {
        let (entry, bytes, index) = versioned(version);
        catalog.install_index(&index).unwrap();
        catalog.install_archive(entry, bytes).unwrap();
    }
    let listing = catalog.listing().unwrap();
    assert_eq!(listing.available.len(), templates::entries().unwrap().len() + 128);
    assert!(catalog.cached_bytes() <= templates::catalog::MAX_CACHE_BYTES);
    assert!(catalog.resolve("generate-text", Some(2), None).is_err());
    assert!(catalog.resolve("generate-text", Some(149), None).is_ok());
    assert!(catalog.resolve("generate-text", Some(1), None).is_ok());
}
#[test]
fn exact_version_add_uses_the_shared_undoable_import_without_network() {
    let (entry, bytes, index) = versioned(2);
    let mut session =
        nodecraft_engine::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([93; 16]), "Offline").unwrap()).unwrap();
    let before = session.document().clone();
    templates::catalog::install_index(&mut session, &index).unwrap();
    templates::catalog::install_archive(&mut session, entry.clone(), bytes).unwrap();
    let (_, _, latest) = versioned(3);
    templates::catalog::install_index(&mut session, &latest).unwrap();
    let value = session.dispatch("template.add", &serde_json::json!({"id":entry.id,"version":2,"blake3":entry.blake3})).unwrap();
    assert_eq!(value["version"], 2);
    session.assets().validate_project(session.document()).unwrap();
    session.dispatch("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!(session.document(), &before);
    let no_host = session.dispatch("template.refresh", &serde_json::json!({})).unwrap_err();
    assert_eq!(no_host.kind, nodecraft_types::ErrorKind::Unsupported);
    assert_eq!(session.document(), &before);
}

#[derive(Default)]
struct Http {
    requests: std::sync::Mutex<Vec<nodecraft_providers::HttpRequest>>,
    replies: std::sync::Mutex<std::collections::VecDeque<nodecraft_providers::HttpResponse>>,
}
impl nodecraft_providers::Http for Http {
    fn send(
        &self,
        _: nodecraft_providers::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        panic!("native template traffic must refuse redirects")
    }
    fn send_no_redirect(
        &self,
        request: nodecraft_providers::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        self.requests.lock().unwrap().push(request);
        let reply = self.replies.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { Ok(reply) })
    }
}
#[test]
fn transport_reads_only_the_index_first_then_the_exact_selected_archive_with_no_credentials() {
    let (entry, bytes, index) = versioned(2);
    let http = Http::default();
    http.replies.lock().unwrap().extend([
        nodecraft_providers::HttpResponse { status: 200, headers: vec![], body: index },
        nodecraft_providers::HttpResponse { status: 200, headers: vec![], body: bytes },
    ]);
    let fetched = futures::executor::block_on(templates::fetch::fetch(&http, templates::fetch::Request::Index)).unwrap();
    assert!(matches!(fetched, templates::fetch::Fetched::Index(_)));
    assert_eq!(http.requests.lock().unwrap().len(), 1, "refresh must not retrieve any archive");
    let fetched = futures::executor::block_on(templates::fetch::fetch(&http, templates::fetch::Request::Archive { entry: entry.clone() })).unwrap();
    assert!(matches!(fetched, templates::fetch::Fetched::Archive { .. }));
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, format!("{}index.json", templates::fetch::ORIGIN));
    assert_eq!(requests[0].max_response_bytes, MAX_INDEX_BYTES);
    assert_eq!(requests[1].url, format!("{}{}", templates::fetch::ORIGIN, entry.file));
    assert_eq!(requests[1].max_response_bytes, MAX_BYTES);
    for request in requests.iter() {
        assert_eq!(request.method, nodecraft_providers::http::HttpMethod::Get);
        assert!(request.body.is_empty() && request.headers.is_empty() && request.secret_headers.is_empty());
        assert_eq!(request.timeout_ms, 10_000);
    }
}
#[test]
fn transport_refuses_malformed_index_changed_archive_redirect_and_unlisted_url_shaping() {
    for (status, body) in [(200, b"bad schema".to_vec()), (302, vec![]), (200, vec![0; MAX_INDEX_BYTES + 1])] {
        let http = Http::default();
        http.replies.lock().unwrap().push_back(nodecraft_providers::HttpResponse { status, headers: vec![], body });
        assert!(futures::executor::block_on(templates::fetch::fetch(&http, templates::fetch::Request::Index)).is_err());
    }
    let (mut entry, _, _) = versioned(2);
    entry.file = "https://elsewhere.invalid/private".into();
    let http = Http::default();
    assert!(futures::executor::block_on(templates::fetch::fetch(&http, templates::fetch::Request::Archive { entry })).is_err());
    assert!(http.requests.lock().unwrap().is_empty());
    let (entry, mut bytes, _) = versioned(2);
    bytes[0] ^= 1;
    http.replies.lock().unwrap().push_back(nodecraft_providers::HttpResponse { status: 200, headers: vec![], body: bytes });
    assert!(futures::executor::block_on(templates::fetch::fetch(&http, templates::fetch::Request::Archive { entry })).is_err());
}

#[test]
fn directory_validator_rejects_digest_mismatch_and_invalid_archive_even_with_its_correct_digest() {
    let (mut entry, bytes, _) = versioned(2);
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let write = |entry: &templates::Entry, bytes: &[u8]| {
        let path = root.join(&entry.file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        let index = serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[entry]})).unwrap();
        std::fs::write(root.join("index.json"), &index).unwrap();
        std::fs::write(root.join("history.json"), index).unwrap();
    };
    write(&entry, &bytes);
    assert_eq!(validate_directory(&root, None).unwrap(), 1);
    let mut corrupt = bytes.clone();
    corrupt[0] ^= 1;
    write(&entry, &corrupt);
    assert!(validate_directory(&root, None).is_err(), "external CI must fail a digest mismatch");
    std::fs::remove_file(root.join(&entry.file)).unwrap();
    entry.blake3 = blake3::hash(&corrupt).to_hex().to_string();
    entry.file = format!("templates/{}/{}/{}.ncraft", entry.id, entry.version, entry.blake3);
    write(&entry, &corrupt);
    assert!(validate_directory(&root, None).is_err(), "a correct digest must not admit an invalid archive");
}
#[test]
fn index_cannot_rewrite_a_known_published_version() {
    let mut catalog = templates::catalog::Catalog::default();
    let (mut entry, _, _) = versioned(1);
    entry.blake3 = "0".repeat(64);
    entry.file = format!("templates/{}/1/{}.ncraft", entry.id, entry.blake3);
    let bytes = serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[entry]})).unwrap();
    assert!(catalog.install_index(&bytes).is_err());
    assert_eq!(catalog.listing().unwrap().stamp, 0);
}

#[test]
fn imported_graph_and_assets_survive_save_reload_without_any_catalog_cache() {
    let mut session =
        nodecraft_engine::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([92; 16]), "Saved example").unwrap()).unwrap();
    session.dispatch("template.add", &serde_json::json!({"id":"edit-image"})).unwrap();
    let snapshot = nodecraft_format::Snapshot { project: session.document().clone(), assets: session.assets().clone(), epoch: 0 };
    let bytes = nodecraft_format::encode(&snapshot).unwrap();
    let loaded = nodecraft_format::decode(&bytes).unwrap();
    let mut fresh = nodecraft_engine::Session::with_storage(loaded, Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
    assert_eq!(fresh.document(), session.document());
    fresh.assets().validate_project(fresh.document()).unwrap();
    assert_eq!(templates::catalog::stamp(&fresh), 0);
    assert_eq!(fresh.dispatch("template.list", &serde_json::json!({})).unwrap()["templates"].as_array().unwrap().len(), templates::entries().unwrap().len());
    assert!(fresh.dispatch("template.add", &serde_json::json!({"id":"edit-image"})).is_err());
}

struct PublishedFixture {
    directory: tempfile::TempDir,
    entries: Vec<templates::Entry>,
    bodies: Vec<Vec<u8>>,
    previous_index: Vec<u8>,
}
impl PublishedFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut entries = vec![];
        let mut bodies = vec![];
        for version in 1..=3 {
            let (entry, body, _) = versioned(version);
            let path = directory.path().join(&entry.file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &body).unwrap();
            entries.push(entry);
            bodies.push(body);
        }
        let previous_index = serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[entries[1]]})).unwrap();
        let fixture = Self { directory, entries, bodies, previous_index };
        fixture.write_indexes();
        fixture
    }
    fn write_indexes(&self) {
        std::fs::write(
            self.directory.path().join("history.json"),
            serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":self.entries})).unwrap(),
        )
        .unwrap();
        std::fs::write(
            self.directory.path().join("index.json"),
            serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[self.entries[2]]})).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn published_history_rejects_deleting_v1_when_current_is_v3_and_previous_is_v2() {
    let fixture = PublishedFixture::new();
    std::fs::remove_file(fixture.directory.path().join(&fixture.entries[0].file)).unwrap();
    let root = fixture.directory.path().canonicalize().unwrap();
    assert!(validate_directory(&root, Some(&fixture.previous_index)).is_err(), "v1 deletion must fail even though only v2/v3 appear in the adjacent indexes");
}
#[test]
fn published_history_rejects_rewriting_v1_when_current_is_v3_and_previous_is_v2() {
    let fixture = PublishedFixture::new();
    let mut changed = fixture.bodies[0].clone();
    changed[0] ^= 1;
    std::fs::write(fixture.directory.path().join(&fixture.entries[0].file), changed).unwrap();
    let root = fixture.directory.path().canonicalize().unwrap();
    assert!(validate_directory(&root, Some(&fixture.previous_index)).is_err(), "v1 replacement must fail after the index has advanced twice");
}
#[test]
fn published_history_rejects_reusing_v1_canvas_in_v3_even_when_v2_has_a_different_canvas() {
    let mut fixture = PublishedFixture::new();
    std::fs::remove_file(fixture.directory.path().join(&fixture.entries[2].file)).unwrap();
    fixture.entries[2].canvas = fixture.entries[0].canvas;
    fixture.entries[2].blake3 = fixture.entries[0].blake3.clone();
    fixture.entries[2].file = format!("templates/{}/3/{}.ncraft", fixture.entries[2].id, fixture.entries[2].blake3);
    std::fs::write(fixture.directory.path().join(&fixture.entries[2].file), &fixture.bodies[0]).unwrap();
    fixture.write_indexes();
    let root = fixture.directory.path().canonicalize().unwrap();
    assert!(validate_directory(&root, Some(&fixture.previous_index)).is_err(), "v3 cannot reuse an older bundled canvas just because immediate v2 differs");
}

#[test]
fn published_history_preserves_all_versions_and_rejects_truncating_the_ledger_itself() {
    let fixture = PublishedFixture::new();
    let root = fixture.directory.path().canonicalize().unwrap();
    let baseline = serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":&fixture.entries[..2]})).unwrap();
    assert_eq!(validate_directory(&root, Some(&baseline)).unwrap(), 1);
    std::fs::write(root.join("history.json"), serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":&fixture.entries[1..]})).unwrap()).unwrap();
    std::fs::remove_file(root.join(&fixture.entries[0].file)).unwrap();
    let error = validate_directory(&root, Some(&baseline)).unwrap_err();
    assert!(error.message.contains("ledger"));
}
#[test]
fn published_history_schema_and_size_bounds_share_the_current_record_contract() {
    let fixture = PublishedFixture::new();
    let history = std::fs::read(fixture.directory.path().join("history.json")).unwrap();
    assert_eq!(templates::parse_history(&history).unwrap().len(), 3);
    assert!(templates::parse_index(&history).is_err(), "current index still permits only one version per ID");
    assert!(templates::parse_history(&vec![b' '; templates::MAX_HISTORY_BYTES + 1]).is_err());
    let mut value: serde_json::Value = serde_json::from_slice(&history).unwrap();
    value["schema_version"] = serde_json::json!(2);
    assert!(templates::parse_history(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn published_history_rejects_nested_canvas_reuse_from_an_older_version() {
    let mut fixture = PublishedFixture::new();
    let root = fixture.directory.path().canonicalize().unwrap();
    for slot in [0, 2] {
        std::fs::remove_file(root.join(&fixture.entries[slot].file)).unwrap();
        let mut snapshot = nodecraft_format::decode(&fixture.bodies[slot]).unwrap();
        nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(nodecraft_types::CanvasId::from_bytes([188; 16]), "Older auxiliary canvas") }
            .apply(&mut snapshot.project)
            .unwrap();
        let bytes = nodecraft_format::encode(&snapshot).unwrap();
        fixture.entries[slot].blake3 = blake3::hash(&bytes).to_hex().to_string();
        fixture.entries[slot].file =
            format!("templates/{}/{}/{}.ncraft", fixture.entries[slot].id, fixture.entries[slot].version, fixture.entries[slot].blake3);
        std::fs::write(root.join(&fixture.entries[slot].file), bytes).unwrap();
    }
    fixture.write_indexes();
    let error = validate_directory(&root, Some(&fixture.previous_index)).unwrap_err();
    assert!(error.message.contains("historical canvas"));
}
#[test]
fn published_history_rejects_unledgered_archives_and_allows_current_description_corrections() {
    let fixture = PublishedFixture::new();
    let root = fixture.directory.path().canonicalize().unwrap();
    let mut current = fixture.entries[2].clone();
    current.description = "Refined directions for this exact version".into();
    std::fs::write(root.join("index.json"), serde_json::to_vec(&serde_json::json!({"schema_version":1,"templates":[current]})).unwrap()).unwrap();
    assert_eq!(validate_directory(&root, Some(&fixture.previous_index)).unwrap(), 1);
    let extra = root.join("templates/unlisted/1/extra.ncraft");
    std::fs::create_dir_all(extra.parent().unwrap()).unwrap();
    std::fs::write(extra, &fixture.bodies[0]).unwrap();
    let error = validate_directory(&root, Some(&fixture.previous_index)).unwrap_err();
    assert!(error.message.contains("inventory"));
}
