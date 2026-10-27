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
    assert!(!h.get_by_label("Refresh templates").accesskit_node().is_disabled(), "{:?}", h.state().session.enabled("template.refresh", &json!({})));
    h.get_by_label("Refresh templates").click();
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
        h.get_by_label("Refresh templates");
        h.get_by_label("Add template");
        h.get_by_label("Retrieve template");
        h.render().unwrap().save(dir.join(format!("{theme:?}-after.png"))).unwrap();
    }
}
