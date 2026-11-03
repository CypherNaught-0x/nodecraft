//! `templates.update` against a local fixture server (#954), never the live repository: a valid
//! update, a digest mismatch, oversize files, a timeout, no network, offline startup with the
//! bundled set, a corrupt cache, the per-update limit, the optional index signature and privacy.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{
    Session,
    templates::{
        self,
        disk::{FileCache, MemoryCache},
        fetch::{Request, SERVICE, Source},
        update,
    },
};
use nodecraft_types::ErrorKind;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
enum Reply {
    Body(u16, Vec<u8>),
    /// Accept the connection and never answer.
    Hang,
}

/// A loopback HTTP/1.1 server: one thread per connection, recording every request head.
struct Fixture {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}
fn serve(routes: HashMap<String, Reply>) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let routes = Arc::new(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let log = Arc::clone(&log);
            let routes = Arc::clone(&routes);
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).is_ok_and(|n| n == 1) {
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").trim_start_matches('/').to_owned();
                log.lock().unwrap().push(head);
                match routes.get(&path).cloned().unwrap_or(Reply::Body(404, b"missing".to_vec())) {
                    Reply::Hang => std::thread::sleep(std::time::Duration::from_secs(30)),
                    Reply::Body(status, body) => {
                        let _ = write!(stream, "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                        let _ = stream.write_all(&body);
                    }
                }
            });
        }
    });
    Fixture { base, seen }
}

/// A new version `version` of a bundled template under `id`, with its own root canvas.
fn versioned(id: &str, version: u32) -> (templates::Entry, Vec<u8>) {
    let mut entry = templates::entries().unwrap().into_iter().find(|entry| entry.id == "generate-text").unwrap();
    let snapshot = templates::load(&entry.id).unwrap();
    let old = snapshot.project.canvases().get(&entry.canvas).unwrap();
    let mut canvas_id = [0xd4; 16];
    canvas_id[8..12].copy_from_slice(&version.to_le_bytes());
    canvas_id[0] = id.bytes().fold(0u8, |sum, b| sum.wrapping_mul(31).wrapping_add(b));
    canvas_id[1] = u8::try_from(id.len()).unwrap();
    let canvas = nodecraft_types::CanvasId::from_bytes(canvas_id);
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([0xd3; 16]), "Fixture template").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Example") }.apply(&mut project).unwrap();
    for node in old.nodes().values() {
        nodecraft_graph::Change::AddNode { canvas, node: node.clone() }.apply(&mut project).unwrap();
    }
    for edge in old.edges().values() {
        nodecraft_graph::Change::Connect { canvas, edge: edge.clone() }.apply(&mut project).unwrap();
    }
    let bytes = nodecraft_format::encode(&nodecraft_format::Snapshot { project, assets: snapshot.assets, epoch: 0 }).unwrap();
    entry.id = id.to_owned();
    entry.canvas = canvas;
    entry.version = version;
    entry.blake3 = blake3::hash(&bytes).to_hex().to_string();
    entry.file = format!("templates/{id}/{version}/{}.ncraft", entry.blake3);
    (entry, bytes)
}
fn index(entries: &[&templates::Entry]) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"templates":entries})).unwrap()
}

fn session() -> Session {
    Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([0x95; 16]), "Updates").unwrap()).unwrap()
}

/// A host like the desktop's: the fetch service queues the request, the "worker" runs it against
/// `source` with the real reqwest transport, and the result is settled on the session.
struct Host {
    session: Session,
    asked: Rc<RefCell<Option<Value>>>,
}
impl Host {
    fn new(mut session: Session) -> Self {
        let asked = Rc::new(RefCell::new(None));
        let queue = Rc::clone(&asked);
        session
            .register_host_service(SERVICE, move |request| {
                *queue.borrow_mut() = Some(request.clone());
                Ok(json!({"pending":1}))
            })
            .unwrap();
        Self { session, asked }
    }
    fn run(&mut self, command: &str, source: &Source) -> Result<(), nodecraft_types::Error> {
        assert_eq!(self.session.dispatch(command, &json!({})).unwrap(), json!({"pending":1}));
        let request: Request = serde_json::from_value(self.asked.borrow_mut().take().unwrap()).unwrap();
        if matches!(request, Request::Update { .. }) {
            update::started(&mut self.session);
            assert_eq!(self.status()["pending"], json!(true));
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let http = nodecraft_providers::ReqwestHttp::new().unwrap();
        let result = runtime.block_on(templates::fetch::fetch_from(&http, source, request));
        templates::fetch::settle(&mut self.session, result)
    }
    fn status(&mut self) -> Value {
        self.session.dispatch("templates.status", &json!({})).unwrap()
    }
    fn available(&mut self, entry: &templates::Entry) -> bool {
        let listing: templates::catalog::Listing = serde_json::from_value(self.session.dispatch("template.list", &json!({})).unwrap()).unwrap();
        listing.available(entry)
    }
}

fn routes(pairs: Vec<(&str, Reply)>) -> HashMap<String, Reply> {
    pairs.into_iter().map(|(path, reply)| (path.to_owned(), reply)).collect()
}

#[test]
fn template_updates954_valid_update_verifies_persists_and_adds_offline_after_restart() {
    let (entry, bytes) = versioned("generate-text", 2);
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, index(&[&entry]))), (entry.file.as_str(), Reply::Body(200, bytes))]));
    let source = Source::loopback(&fixture.base).unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut host = Host::new(session());
    assert!(templates::disk::install(&mut host.session, Box::new(FileCache::new(data.path()))).is_empty());
    update::configure(&mut host.session, &source);
    host.run("templates.update", &source).unwrap();
    let status = host.status();
    assert_eq!(status["pending"], json!(false));
    assert_eq!(status["source"], json!(fixture.base));
    assert_eq!(status["last"]["error"], Value::Null);
    assert_eq!(status["last"]["listed"], json!(1));
    assert_eq!(status["last"]["downloaded"], json!([{"id":entry.id,"version":2,"blake3":entry.blake3}]));
    assert_eq!(status["cache"]["files"], json!(1));
    assert_eq!(status["cache"]["persistent"], json!(true));
    let dir = data.path().join("templates").join(templates::disk::LAYOUT);
    assert!(dir.join(format!("{}.ncraft", entry.blake3)).is_file() && dir.join(format!("{}.json", entry.blake3)).is_file() && dir.join("index.json").is_file());
    // A second update finds nothing changed and downloads nothing.
    let before = fixture.seen.lock().unwrap().len();
    host.run("templates.update", &source).unwrap();
    assert_eq!(fixture.seen.lock().unwrap().len(), before + 1, "only the index is read again");
    assert_eq!(host.status()["last"]["downloaded"], json!([]));

    // A restart with no network: the cache alone makes v2 available, and adding it copies it in.
    let mut restarted = session();
    assert!(templates::disk::install(&mut restarted, Box::new(FileCache::new(data.path()))).is_empty());
    let before = restarted.document().clone();
    let added = restarted.dispatch("template.add", &json!({"id":entry.id,"version":2,"blake3":entry.blake3})).unwrap();
    assert_eq!(added["version"], json!(2));
    assert!(restarted.document().canvases().contains_key(&entry.canvas));
    restarted.assets().validate_project(restarted.document()).unwrap();
    restarted.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(restarted.document(), &before);
    assert_eq!(restarted.dispatch("templates.update", &json!({})).unwrap_err().kind, ErrorKind::Unsupported, "no host, no network");
}

#[test]
fn template_updates954_requests_carry_only_the_generic_user_agent() {
    let (entry, bytes) = versioned("generate-text", 2);
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, index(&[&entry]))), (entry.file.as_str(), Reply::Body(200, bytes))]));
    let source = Source::loopback(&fixture.base).unwrap();
    let mut host = Host::new(session());
    host.run("templates.update", &source).unwrap();
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].starts_with("GET /index.json HTTP/1.1\r\n"), "{}", seen[0]);
    assert!(seen[1].starts_with(&format!("GET /{} HTTP/1.1\r\n", entry.file)), "{}", seen[1]);
    for head in seen.iter() {
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains(&format!("user-agent: {}\r\n", nodecraft_providers::USER_AGENT.to_ascii_lowercase())), "{head}");
        for private in ["cookie", "authorization", "x-api-key", "referer"] {
            assert!(!lower.contains(&format!("{private}:")), "{head}");
        }
    }
}

#[test]
fn template_updates954_digest_mismatch_is_rejected_reported_and_keeps_the_last_good_copy() {
    let data = tempfile::tempdir().unwrap();
    let (two, good) = versioned("generate-text", 2);
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, index(&[&two]))), (two.file.as_str(), Reply::Body(200, good))]));
    let mut host = Host::new(session());
    templates::disk::install(&mut host.session, Box::new(FileCache::new(data.path())));
    host.run("templates.update", &Source::loopback(&fixture.base).unwrap()).unwrap();
    assert!(host.available(&two));

    // v3 is published, but the server sends different bytes than its digest names.
    let (three, mut tampered) = versioned("generate-text", 3);
    tampered[0] ^= 1;
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, index(&[&three]))), (three.file.as_str(), Reply::Body(200, tampered))]));
    host.run("templates.update", &Source::loopback(&fixture.base).unwrap()).unwrap();
    let status = host.status();
    assert_eq!(status["last"]["downloaded"], json!([]));
    assert_eq!(status["last"]["failed"][0]["id"], json!("generate-text"));
    assert_eq!(status["last"]["failed"][0]["version"], json!(3));
    assert!(status["last"]["failed"][0]["error"].as_str().unwrap().contains("digest"), "{status}");
    assert!(!host.available(&three));
    assert!(host.available(&two), "the last good copy stays");
    let dir = data.path().join("templates").join(templates::disk::LAYOUT);
    assert!(!dir.join(format!("{}.ncraft", three.blake3)).exists(), "a rejected archive is never stored");
    // Without a version, adding picks the newest verified copy: v2.
    assert_eq!(host.session.dispatch("template.add", &json!({"id":"generate-text"})).unwrap()["version"], json!(2));

    // A published version may not change its content.
    let (mut rewritten, bytes) = versioned("generate-text", 4);
    rewritten.version = 2;
    rewritten.file = format!("templates/generate-text/2/{}.ncraft", rewritten.blake3);
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, index(&[&rewritten]))), (rewritten.file.as_str(), Reply::Body(200, bytes))]));
    let error = host.run("templates.update", &Source::loopback(&fixture.base).unwrap()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(fixture.seen.lock().unwrap().len(), 1, "nothing is downloaded after a conflicting index");
    assert!(host.available(&two));
}

#[test]
fn template_updates954_oversize_index_and_archive_are_refused() {
    let mut host = Host::new(session());
    let huge = vec![b' '; templates::MAX_INDEX_BYTES + 1];
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, huge))]));
    let error = host.run("templates.update", &Source::loopback(&fixture.base).unwrap()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput, "{error:?}");
    assert!(host.status()["last"]["error"].as_str().unwrap().contains("size limit"));
    assert_eq!(templates::catalog::stamp(&host.session), 0, "nothing changed");

    let (entry, _) = versioned("generate-text", 2);
    let fixture =
        serve(routes(vec![("index.json", Reply::Body(200, index(&[&entry]))), (entry.file.as_str(), Reply::Body(200, vec![0; templates::MAX_BYTES + 1]))]));
    host.run("templates.update", &Source::loopback(&fixture.base).unwrap()).unwrap();
    let status = host.status();
    assert_eq!(status["last"]["failed"][0]["version"], json!(2), "{status}");
    assert!(!host.available(&entry));
}

#[test]
fn template_updates954_timeout_is_reported_and_settles() {
    let mut host = Host::new(session());
    let fixture = serve(routes(vec![("index.json", Reply::Hang)]));
    let source = Source::loopback(&fixture.base).unwrap().with_timeout_ms(300);
    let started = std::time::Instant::now();
    let error = host.run("templates.update", &source).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(error.kind, ErrorKind::Timeout, "{error:?}");
    let status = host.status();
    assert_eq!(status["pending"], json!(false));
    assert!(status["last"]["error"].as_str().unwrap().contains("in time"));
    assert!(host.available(&templates::entries().unwrap()[0]), "the bundled set stays");
}

#[test]
fn template_updates954_no_network_keeps_the_bundled_set() {
    // A port nothing listens on.
    let base = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}/", listener.local_addr().unwrap().port())
    };
    let mut host = Host::new(session());
    let error = host.run("templates.update", &Source::loopback(&base).unwrap()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Network, "{error:?}");
    assert!(host.status()["last"]["error"].as_str().unwrap().contains("could not be read"));
    for entry in templates::entries().unwrap() {
        assert!(host.available(&entry));
    }
}

#[test]
fn template_updates954_offline_startup_lists_and_adds_the_bundled_set() {
    let data = tempfile::tempdir().unwrap();
    let mut session = session();
    // No host fetch service and an empty cache folder: startup never touches the network.
    assert!(templates::disk::install(&mut session, Box::new(FileCache::new(&data.path().join("absent")))).is_empty());
    let listing: templates::catalog::Listing = serde_json::from_value(session.dispatch("template.list", &json!({})).unwrap()).unwrap();
    assert_eq!(listing.available.len(), templates::entries().unwrap().len());
    assert_eq!(session.dispatch("template.add", &json!({"id":"generate-text"})).unwrap()["version"], json!(1));
    let status = session.dispatch("templates.status", &json!({})).unwrap();
    assert_eq!(status["supported"], json!(false));
    assert_eq!(status["last"], Value::Null);
    assert_eq!(status["bundled"], json!(templates::entries().unwrap().len()));
    for command in ["templates.update", "templates.clear_cache"] {
        assert_eq!(session.dispatch(command, &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
    }
    assert!(!data.path().join("absent").exists(), "reading an absent cache creates nothing");
}

#[test]
fn template_updates954_corrupt_cache_files_are_reported_skipped_and_removable() {
    let data = tempfile::tempdir().unwrap();
    let dir = data.path().join("templates").join(templates::disk::LAYOUT);
    std::fs::create_dir_all(dir.join("a-folder")).unwrap();
    let (good, bytes) = versioned("generate-text", 2);
    let (bad, mut tampered) = versioned("generate-text", 3);
    tampered[0] ^= 1;
    std::fs::write(dir.join(format!("{}.ncraft", good.blake3)), &bytes).unwrap();
    std::fs::write(dir.join(format!("{}.json", good.blake3)), serde_json::to_vec(&good).unwrap()).unwrap();
    std::fs::write(dir.join(format!("{}.ncraft", bad.blake3)), &tampered).unwrap();
    std::fs::write(dir.join(format!("{}.json", bad.blake3)), serde_json::to_vec(&bad).unwrap()).unwrap();
    std::fs::write(dir.join(format!("{}.json", "0".repeat(64))), b"{not json").unwrap();
    std::fs::write(dir.join("orphan.ncraft"), b"zip?").unwrap();
    std::fs::write(dir.join("index.json"), b"\xff\xfe garbage").unwrap();
    std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
    // A record renamed to another digest is not trusted either.
    std::fs::write(dir.join(format!("{}.ncraft", "a".repeat(64))), &bytes).unwrap();
    std::fs::write(dir.join(format!("{}.json", "a".repeat(64))), serde_json::to_vec(&good).unwrap()).unwrap();

    let mut host = Host::new(session());
    let problems = templates::disk::install(&mut host.session, Box::new(FileCache::new(data.path())));
    assert!(problems.len() >= 6, "{problems:#?}");
    assert!(problems.iter().any(|problem| problem.contains("digest")), "{problems:#?}");
    assert!(problems.iter().any(|problem| problem.contains("index")), "{problems:#?}");
    assert!(host.available(&good), "the intact file is still used");
    assert!(!host.available(&bad));
    for entry in templates::entries().unwrap() {
        assert!(host.available(&entry));
    }
    let status = host.status();
    assert!(!status["cache"]["problems"].as_array().unwrap().is_empty());
    assert_eq!(status["cache"]["location"], json!(dir.display().to_string()));

    host.run("templates.clear_cache", &Source::official()).unwrap();
    assert!(!data.path().join("templates").exists());
    assert!(!host.available(&good));
    assert_eq!(host.status()["cache"]["problems"], json!([]));
    assert_eq!(host.status()["cache"]["files"], json!(0));
    for entry in templates::entries().unwrap() {
        assert!(host.available(&entry), "bundled templates stay");
    }
}

#[test]
fn template_updates954_one_update_downloads_at_most_the_limit() {
    let made: Vec<_> = (0..update::MAX_PER_UPDATE + 2).map(|n| versioned(&format!("fixture-{n}"), 1)).collect();
    let entries: Vec<_> = made.iter().map(|(entry, _)| entry).collect();
    let mut pairs = vec![("index.json".to_owned(), Reply::Body(200, index(&entries)))];
    pairs.extend(made.iter().map(|(entry, bytes)| (entry.file.clone(), Reply::Body(200, bytes.clone()))));
    let fixture = serve(pairs.into_iter().collect());
    let source = Source::loopback(&fixture.base).unwrap();
    let mut host = Host::new(session());
    templates::disk::install(&mut host.session, Box::new(MemoryCache::default()));
    host.run("templates.update", &source).unwrap();
    let status = host.status();
    assert_eq!(status["last"]["downloaded"].as_array().unwrap().len(), update::MAX_PER_UPDATE);
    assert_eq!(status["last"]["skipped"], json!(2));
    assert_eq!(fixture.seen.lock().unwrap().len(), 1 + update::MAX_PER_UPDATE);
    host.run("templates.update", &source).unwrap();
    assert_eq!(host.status()["last"]["downloaded"].as_array().unwrap().len(), 2, "the next update takes the rest");
    assert_eq!(host.status()["last"]["skipped"], json!(0));
}

#[test]
fn template_updates954_signed_index_is_checked_when_the_source_has_keys() {
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let (entry, bytes) = versioned("generate-text", 2);
    let body = index(&[&entry]);
    let mut message = update::SIGNATURE_CONTEXT.to_vec();
    message.extend_from_slice(&body);
    let good = key.sign(&message).to_bytes().to_vec();
    let fixture = serve(routes(vec![
        ("index.json", Reply::Body(200, body.clone())),
        ("index.json.sig", Reply::Body(200, good)),
        (entry.file.as_str(), Reply::Body(200, bytes.clone())),
    ]));
    let source = Source::loopback(&fixture.base).unwrap().with_keys(vec![key.verifying_key()]);
    let mut host = Host::new(session());
    host.run("templates.update", &source).unwrap();
    assert_eq!(host.status()["last"]["signed"], json!(true));
    assert!(host.available(&entry));

    let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    for signature in [other.sign(&message).to_bytes().to_vec(), vec![1, 2, 3]] {
        let fixture = serve(routes(vec![("index.json", Reply::Body(200, body.clone())), ("index.json.sig", Reply::Body(200, signature))]));
        let mut host = Host::new(session());
        let error = host.run("templates.update", &Source::loopback(&fixture.base).unwrap().with_keys(vec![key.verifying_key()])).unwrap_err();
        assert!(error.message.contains("signature"), "{error:?}");
        assert!(!host.available(&entry));
    }
    // A missing signature is refused too.
    let fixture = serve(routes(vec![("index.json", Reply::Body(200, body))]));
    let mut host = Host::new(session());
    assert!(host.run("templates.update", &Source::loopback(&fixture.base).unwrap().with_keys(vec![key.verifying_key()])).is_err());
}

#[test]
fn template_updates954_sources_are_https_official_or_a_loopback_fixture() {
    assert!(Source::official().base().starts_with("https://raw.githubusercontent.com/CypherNaught-0x/nodecraft-templates/"));
    for refused in
        ["http://example.com/", "https://127.0.0.1:1/", "http://127.0.0.1:1", "http://127.0.0.1:/", "http://127.0.0.1:1/../", "http://127.0.0.1:123456/"]
    {
        assert!(Source::loopback(refused).is_err(), "{refused}");
    }
    assert!(Source::loopback("http://127.0.0.1:8080/").is_ok());
}

#[test]
fn template_updates954_hostile_update_requests_are_errors() {
    let identity = json!({"id":"x","version":1,"blake3":"0".repeat(64)});
    let many: Vec<Value> = (0..600).map(|_| identity.clone()).collect();
    let request: Request = serde_json::from_value(json!({"kind":"update","known":many,"have":[]})).unwrap();
    let http = nodecraft_providers::ReqwestHttp::new().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    // Refused before any request: the source is a port nothing listens on, which would be a Network error.
    let error = runtime.block_on(templates::fetch::fetch_from(&http, &Source::loopback("http://127.0.0.1:9/").unwrap(), request)).err().unwrap();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    for params in [json!({"x":1}), json!("x")] {
        let mut host = Host::new(session());
        assert!(host.session.dispatch("templates.update", &params).is_err());
        assert!(host.session.dispatch("templates.status", &params).is_err());
        assert!(host.session.dispatch("templates.clear_cache", &params).is_err());
    }
}
