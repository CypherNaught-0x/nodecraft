//! Golden graphs (#79): small canvases run end to end against the mock provider. Bless with
//! `NODECRAFT_BLESS=1 cargo test -p nodecraft-testkit --test golden`.

use nodecraft_exec::planner::RunMode;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Project};
use nodecraft_nodes::router;
use nodecraft_providers::{ErrorKind, Provider};
use nodecraft_testkit::golden;
use nodecraft_testkit::kinds::registry;
use nodecraft_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use nodecraft_types::{CanvasId, EdgeId, Modality, NodeId, ProjectId, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

const C: CanvasId = CanvasId::from_bytes([1; 16]);

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

/// A node: number, kind, title and text parameters.
type NodeSpec<'a> = (u8, &'a str, &'a str, &'a [(&'a str, &'a str)]);

/// A project from nodes and `(from, from port, to, order)` wires.
fn project(nodes: &[NodeSpec<'_>], wires: &[(u8, &str, u8, u32)]) -> Project {
    let kinds = registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Golden").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut changes = Vec::new();
    for (n, kind, title, params) in nodes {
        let values = params.iter().map(|(k, v)| ((*k).to_owned(), Value::Text((*v).into()))).collect();
        let config = if *kind == router::ID { router::config(Modality::Text) } else { Default::default() };
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id(*n), config, &values).unwrap();
        node.title = (*title).into();
        changes.push(Change::AddNode { canvas: C, node });
    }
    for (e, (from, port, to, order)) in wires.iter().enumerate() {
        let edge = Edge {
            id: EdgeId::from_bytes([100 + u8::try_from(e).unwrap(); 16]),
            from: Endpoint { node: id(*from), port: (*port).into() },
            to: Endpoint { node: id(*to), port: "in".into() },
            order: *order,
        };
        changes.push(Change::Connect { canvas: C, edge });
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    project
}

/// Runs `requested` with upstream, checks the golden report and project, and that recording
/// the results left nothing stale.
fn golden(name: &str, project: &Project, requested: &[u8], providers: &[Arc<dyn Provider>]) {
    let kinds = registry().unwrap();
    let requested: BTreeSet<NodeId> = requested.iter().map(|n| id(*n)).collect();
    golden::check(&dir(), &format!("{name}.project"), &serde_json::to_value(project).unwrap()).unwrap();
    let outcome = golden::run(project, C, &requested, &RunMode::WithUpstream, &kinds, providers).unwrap();
    golden::check(&dir(), name, &outcome.report).unwrap();
    let after = nodecraft_exec::staleness::stale(outcome.project.canvases().get(&C).unwrap(), &kinds).unwrap();
    assert!(after.is_empty(), "{name}: stale after the run: {after:?}");
}

fn standard() -> Vec<Arc<dyn Provider>> {
    vec![Arc::new(MockProvider::standard())]
}

#[test]
fn a_prompt_feeds_a_text_model_that_feeds_an_image_model() {
    let p = project(
        &[(1, "source.text", "Idea", &[("text", "a fox in the snow")]), (2, "mock.text", "Writer", &[("style", "ink")]), (3, "mock.image", "Painter", &[])],
        &[(1, "out", 2, 0), (2, "out", 3, 0)],
    );
    golden("prompt_chain", &p, &[3], &standard());
}

#[test]
fn a_router_fans_two_prompts_into_one_model_in_order() {
    let p = project(
        &[
            (1, "source.text", "Subject", &[("text", "an owl")]),
            (2, "source.text", "Mood", &[("text", "at dusk")]),
            (3, router::ID, "Hub", &[]),
            (4, "mock.text", "Writer", &[]),
        ],
        &[(1, "out", 3, 0), (2, "out", 3, 1), (3, "out", 4, 0)],
    );
    golden("router_fan_in", &p, &[4], &standard());
}

#[test]
fn a_failing_model_skips_what_comes_after_it() {
    let failing = MockProvider::new(
        "mock",
        [
            ("mock-text".to_owned(), MockModel { polls: 1, fail: Some(MockFailure::Job(ErrorKind::JobFailed)), ..MockModel::new(MockOutput::Text) }),
            ("mock-image".to_owned(), MockModel::new(MockOutput::Image)),
        ],
    );
    let p = project(
        &[(1, "source.text", "Idea", &[("text", "a fox")]), (2, "mock.text", "Writer", &[]), (3, "mock.image", "Painter", &[])],
        &[(1, "out", 2, 0), (2, "out", 3, 0)],
    );
    golden("failure_skips_downstream", &p, &[3], &[Arc::new(failing)]);
}
