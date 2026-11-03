use super::*;
use crate::app::NodeCraftApp;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::templates::{
    self,
    catalog::{Identity, Listing},
};

fn remote() -> (templates::Entry, Vec<u8>, Vec<u8>) {
    let mut entry = templates::entries().unwrap().into_iter().find(|entry| entry.id == "generate-text").unwrap();
    let snapshot = templates::load(&entry.id).unwrap();
    let old = snapshot.project.canvases().get(&entry.canvas).unwrap();
    let canvas = nodecraft_types::CanvasId::from_bytes([224; 16]);
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([223; 16]), "New template version").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Example") }.apply(&mut project).unwrap();
    for node in old.nodes().values() {
        nodecraft_graph::Change::AddNode { canvas, node: node.clone() }.apply(&mut project).unwrap();
    }
    for edge in old.edges().values() {
        nodecraft_graph::Change::Connect { canvas, edge: edge.clone() }.apply(&mut project).unwrap();
    }
    let bytes = nodecraft_format::encode(&nodecraft_format::Snapshot { project, assets: snapshot.assets, epoch: 0 }).unwrap();
    entry.canvas = canvas;
    entry.blake3 = blake3::hash(&bytes).to_hex().to_string();
    entry.version = 2;
    entry.file = format!("templates/{}/2/{}.ncraft", entry.id, entry.blake3);
    let index = serde_json::to_vec(&json!({"schema_version":1,"templates":[entry]})).unwrap();
    (entry, bytes, index)
}
fn setup() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    app.library.open = true;
    app.library.template_query = "Generate text".into();
    app
}
#[test]
fn templates953_pointer_refresh_retrieves_only_selected_version_and_adds_it_as_one_undo_step() {
    let (entry, bytes, index) = remote();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&calls);
    let mut fetcher = TemplateFetcher::asynchronous(Box::new(move |request| {
        let result = match request {
            Request::Index => {
                observed.borrow_mut().push("index");
                Fetched::Index(index.clone())
            }
            Request::Archive { entry } => {
                observed.borrow_mut().push("archive");
                Fetched::Archive { entry, bytes: bytes.clone() }
            }
            other => panic!("unexpected template request {other:?}"),
        };
        Box::pin(async move { Ok(result) })
    }));
    let mut app = setup();
    fetcher.bind(&mut app.session).unwrap();
    app.template_fetch = Some(fetcher);
    let before = app.session.document().clone();
    let mut h = Harness::builder().with_size(egui::vec2(1100., 800.)).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            app.tick_template_fetch(ui.ctx());
            app.show(ui);
        },
        app,
    );
    h.run();
    assert!(!h.get_by_label("Update templates").accesskit_node().is_disabled(), "{:?}", h.state().session.enabled("templates.update", &json!({})));
    h.state_mut().session.dispatch("template.refresh", &json!({})).unwrap();
    h.run();
    h.run();
    h.run();
    assert_eq!(&*calls.borrow(), &["index"], "notice={:?}, busy={}", h.state().notice, h.state().template_fetch.as_ref().unwrap().waiting());
    assert_eq!(h.state().session.document(), &before);
    h.get_by_label("Generate text · v2");
    h.get_by_label("Retrieve template").click();
    h.run();
    h.run();
    h.run();
    assert_eq!(&*calls.borrow(), &["index", "archive"]);
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.get_all_by_label("Add template").count(), 2);
    h.get_all_by_label("Add template").nth(1).unwrap().click();
    h.run();
    assert!(h.state().session.document().canvases().contains_key(&entry.canvas), "{:?}", h.state().notice);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    assert_eq!(h.state().session.document(), &before);
}
#[test]
fn templates953_stale_queued_request_never_starts_in_a_replacement_session() {
    let calls = Rc::new(RefCell::new(0));
    let observed = Rc::clone(&calls);
    let mut fetcher = TemplateFetcher::asynchronous(Box::new(move |_| {
        *observed.borrow_mut() += 1;
        Box::pin(async { Err(Error::new(ErrorKind::Network, "offline")) })
    }));
    let mut old = setup().session;
    fetcher.bind(&mut old).unwrap();
    old.dispatch("template.refresh", &json!({})).unwrap();
    let mut replacement = setup().session;
    fetcher.tick(&mut replacement, &egui::Context::default());
    assert_eq!(*calls.borrow(), 0);
    assert_eq!(templates::catalog::stamp(&replacement), 0);
}
#[test]
fn templates953_single_flight_retains_busy_until_stale_completion_and_never_imports() {
    let (_, _, index) = remote();
    let response = Rc::new(RefCell::new(None::<Result<Fetched, Error>>));
    let observed = Rc::clone(&response);
    let mut fetcher = TemplateFetcher::asynchronous(Box::new(move |_| {
        let response = Rc::clone(&observed);
        Box::pin(std::future::poll_fn(move |_| match response.borrow_mut().take() {
            Some(result) => std::task::Poll::Ready(result),
            None => std::task::Poll::Pending,
        }))
    }));
    let mut old = setup().session;
    fetcher.bind(&mut old).unwrap();
    old.dispatch("template.refresh", &json!({})).unwrap();
    assert!(old.dispatch("template.refresh", &json!({})).is_err());
    let ctx = egui::Context::default();
    fetcher.tick(&mut old, &ctx);
    let mut replacement = setup().session;
    let before = replacement.document().clone();
    fetcher.tick(&mut replacement, &ctx);
    assert!(replacement.dispatch("template.refresh", &json!({})).is_err());
    *response.borrow_mut() = Some(Ok(Fetched::Index(index)));
    assert!(fetcher.tick(&mut replacement, &ctx).unwrap().contains("discarded"));
    assert_eq!(templates::catalog::stamp(&replacement), 0);
    assert_eq!(replacement.document(), &before);
    assert!(!fetcher.waiting());
}
#[test]
fn templates953_catalog_stamp_and_session_identity_refresh_the_panel_cache() {
    let (entry, _, index) = remote();
    let app = setup();
    let mut h = Harness::builder().with_size(egui::vec2(1100., 800.)).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            app.tick_template_fetch(ui.ctx());
            app.show(ui);
        },
        app,
    );
    h.run();
    templates::catalog::install_index(&mut h.state_mut().session, &index).unwrap();
    h.run();
    h.get_by_label("Generate text · v2");
    let value = h.state_mut().session.dispatch("template.list", &json!({})).unwrap();
    let list: Listing = serde_json::from_value(value).unwrap();
    assert!(!list.available.contains(&Identity::from(&entry)));
    h.state_mut().session = setup().session;
    h.run();
    assert!(h.query_by_label("Generate text · v2").is_none());
    h.get_by_label("Generate text · v1");
}

#[test]
fn templates954_pointer_update_shows_status_then_clear_removes_downloads() {
    let (entry, bytes, index) = remote();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&asked);
    let mut fetcher = TemplateFetcher::asynchronous(Box::new(move |request| {
        let result = match request {
            Request::Update { have, .. } => {
                observed.borrow_mut().push("update");
                assert!(!have.contains(&Identity::from(&entry)));
                Ok(Fetched::Update(Box::new(nodecraft_engine::templates::update::Updated {
                    index: index.clone(),
                    signature: None,
                    archives: vec![(entry.clone(), bytes.clone())],
                    failed: Vec::new(),
                    skipped: 0,
                })))
            }
            Request::ClearCache => {
                observed.borrow_mut().push("clear");
                Ok(Fetched::ClearCache)
            }
            other => panic!("unexpected template request {other:?}"),
        };
        Box::pin(async move { result })
    }));
    let mut app = setup();
    nodecraft_engine::templates::disk::install(&mut app.session, Box::new(nodecraft_engine::templates::disk::MemoryCache::default()));
    fetcher.bind(&mut app.session).unwrap();
    app.template_fetch = Some(fetcher);
    let before = app.session.document().clone();
    let mut h = Harness::builder().with_size(egui::vec2(1100., 800.)).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            app.tick_template_fetch(ui.ctx());
            app.show(ui);
        },
        app,
    );
    h.run();
    h.get_by_label("Using the bundled templates. Update to get new and changed ones.");
    assert!(h.get_by_label("Clear downloaded").accesskit_node().is_disabled(), "nothing downloaded yet");
    h.get_by_label("Update templates").click();
    h.run();
    h.run();
    h.run();
    assert_eq!(&*asked.borrow(), &["update"], "notice={:?}", h.state().notice);
    assert!(h.get_all_by_label("1 template was downloaded and verified.").next().is_some(), "{:?}", h.state().notice);
    let status = h.state_mut().session.dispatch("templates.status", &json!({})).unwrap();
    assert_eq!(status["pending"], json!(false));
    assert_eq!(status["cache"]["files"], json!(1));
    assert_eq!(h.state().session.document(), &before, "updating never edits the project");
    assert_eq!(h.get_all_by_label("Add template").count(), 2);
    h.get_by_label("Clear downloaded").click();
    h.run();
    h.run();
    h.run();
    assert_eq!(&*asked.borrow(), &["update", "clear"]);
    assert_eq!(nodecraft_engine::templates::catalog::cached(&h.state().session), 0);
    assert_eq!(h.get_all_by_label("Add template").count(), 1, "bundled stays");
}
#[test]
fn templates954_failed_update_is_shown_and_cache_problems_are_reported() {
    let mut fetcher =
        TemplateFetcher::asynchronous(Box::new(|_| Box::pin(async { Err(Error::new(ErrorKind::Timeout, "The template repository didn't answer in time")) })));
    let mut app = setup();
    let cache = nodecraft_engine::templates::disk::MemoryCache::default();
    use nodecraft_engine::templates::disk::TemplateCacheStore;
    cache.write_archive(&"0".repeat(64), b"{broken", b"not a zip").unwrap();
    let problems = nodecraft_engine::templates::disk::install(&mut app.session, Box::new(cache));
    assert_eq!(problems.len(), 1);
    fetcher.bind(&mut app.session).unwrap();
    app.template_fetch = Some(fetcher);
    let mut h = Harness::builder().with_size(egui::vec2(1100., 800.)).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            app.tick_template_fetch(ui.ctx());
            app.show(ui);
        },
        app,
    );
    h.run();
    assert!(h.query_by_label_contains("Template cache:").is_some());
    h.get_by_label("Update templates").click();
    h.run();
    h.run();
    h.run();
    assert!(h.get_all_by_label("Templates were not updated: The template repository didn't answer in time").next().is_some(), "{:?}", h.state().notice);
    h.get_all_by_label("Add template").next().unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn templates954_worker_reads_the_cache_off_the_ui_thread_and_applies_writes() {
    use nodecraft_engine::templates::disk::{self, FileCache, TemplateCacheStore};
    let data = tempfile::tempdir().unwrap();
    let (entry, bytes, index) = remote();
    let cache = FileCache::new(data.path());
    cache.write_index(&index, None).unwrap();
    cache.write_archive(&entry.blake3, &serde_json::to_vec(&entry).unwrap(), &bytes).unwrap();
    let mut fetcher = TemplateFetcher::spawn(
        Source::official(),
        Box::new(|request| match request {
            Request::ClearCache => Ok(Fetched::ClearCache),
            _ => Err(Error::new(ErrorKind::Network, "offline fixture")),
        }),
        Some(cache),
    )
    .unwrap();
    let mut session = setup().session;
    fetcher.bind(&mut session).unwrap();
    // Until the worker hands the verified cache back, an update would re-download it.
    if disk::is_loading(&session) {
        assert_eq!(session.dispatch("templates.update", &json!({})).unwrap_err().kind, ErrorKind::Busy);
    }
    let ctx = egui::Context::default();
    let wait = |fetcher: &mut TemplateFetcher, session: &mut Session, done: &dyn Fn(&Session) -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done(session) {
            assert!(std::time::Instant::now() < deadline, "the template worker didn't answer");
            fetcher.tick(session, &ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    wait(&mut fetcher, &mut session, &|session| !disk::is_loading(session));
    let listing: Listing = serde_json::from_value(session.dispatch("template.list", &json!({})).unwrap()).unwrap();
    assert!(listing.available(&entry), "the cached version is verified and listed");
    assert!(disk::problems(&session).is_empty(), "{:?}", disk::problems(&session));
    session.dispatch("templates.clear_cache", &json!({})).unwrap();
    let folder = data.path().join("templates").join(disk::LAYOUT);
    // Success is announced only once the worker has removed the folder.
    let notices = std::cell::RefCell::new(Vec::new());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !notices.borrow().iter().any(|notice: &String| notice.starts_with("Downloaded templates were removed")) {
        assert!(std::time::Instant::now() < deadline, "no removal notice: {:?}", notices.borrow());
        if let Some(notice) = fetcher.tick(&mut session, &ctx) {
            if notice.starts_with("Downloaded templates were removed") {
                assert!(!folder.exists(), "success before the folder was removed");
            }
            notices.borrow_mut().push(notice);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!disk::is_clearing(&session));
    assert_eq!(templates::catalog::cached(&session), 0);
}

// Unix: removing a plain file with remove_dir_all fails there; Windows reports it differently.
#[cfg(unix)]
#[test]
fn templates954_a_failed_worker_clear_is_reported_not_announced_as_done() {
    use nodecraft_engine::templates::disk::{self, FileCache};
    let data = tempfile::tempdir().unwrap();
    // A plain file where the cache folder belongs: reading reports it, removing it fails.
    std::fs::create_dir_all(data.path().join("templates")).unwrap();
    std::fs::write(data.path().join("templates").join(disk::LAYOUT), b"not a folder").unwrap();
    let mut fetcher = TemplateFetcher::spawn(
        Source::official(),
        Box::new(|request| match request {
            Request::ClearCache => Ok(Fetched::ClearCache),
            _ => Err(Error::new(ErrorKind::Network, "offline fixture")),
        }),
        Some(FileCache::new(data.path())),
    )
    .unwrap();
    let mut session = setup().session;
    fetcher.bind(&mut session).unwrap();
    let ctx = egui::Context::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while disk::is_loading(&session) {
        assert!(std::time::Instant::now() < deadline);
        fetcher.tick(&mut session, &ctx);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    session.dispatch("templates.clear_cache", &json!({})).unwrap();
    let mut notices = Vec::new();
    while !notices.iter().any(|notice: &String| notice.contains("couldn't be removed")) {
        assert!(std::time::Instant::now() < deadline, "no failure notice: {notices:?}");
        if let Some(notice) = fetcher.tick(&mut session, &ctx) {
            notices.push(notice);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!notices.iter().any(|notice| notice.starts_with("Downloaded templates were removed")), "{notices:?}");
    assert!(!disk::is_clearing(&session));
    let status = session.dispatch("templates.status", &json!({})).unwrap();
    assert!(status["cache"]["problems"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("couldn't be removed")), "{status}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn templates954_a_panicking_cache_read_ends_the_startup_wait() {
    use nodecraft_engine::templates::disk;
    let loaded = load_guarded(|| panic!("simulated cache read panic"));
    let mut session = setup().session;
    disk::install_deferred(&mut session, true, None);
    session.register_host_service(SERVICE, |_| Ok(json!({"pending":1}))).unwrap();
    assert_eq!(session.dispatch("templates.update", &json!({})).unwrap_err().kind, ErrorKind::Busy);
    let problems = disk::adopt(&mut session, loaded);
    assert!(problems.iter().any(|problem| problem.contains("stopped unexpectedly")), "{problems:?}");
    assert!(!disk::is_loading(&session));
    assert_eq!(session.dispatch("templates.update", &json!({})).unwrap(), json!({"pending":1}));
}

#[test]
#[ignore = "native GPU evidence; TEMPLATE953_RENDER_DIR"]
fn templates953_library_render() {
    let dir = std::path::PathBuf::from(std::env::var_os("TEMPLATE953_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let (_, _, index) = remote();
        let mut app = setup();
        app.theme = theme;
        templates::catalog::install_index(&mut app.session, &index).unwrap();
        let mut fetcher = TemplateFetcher::asynchronous(Box::new(|_| Box::pin(async { Err(Error::new(ErrorKind::Network, "offline fixture")) })));
        fetcher.bind(&mut app.session).unwrap();
        app.template_fetch = Some(fetcher);
        let mut h = Harness::builder().wgpu().with_size(egui::vec2(1100., 800.)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        crate::theme::apply(&h.ctx, theme);
        h.run();
        h.get_by_label("Update templates");
        h.get_by_label("Add template");
        h.get_by_label("Retrieve template");
        h.render().unwrap().save(dir.join(format!("{theme:?}-after.png"))).unwrap();
    }
}
