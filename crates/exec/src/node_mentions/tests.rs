use super::*;
use crate::planner::{InputSource, Planned, Reason, RunMode, plan};
use nodecraft_graph::{Change, Edge, Endpoint, Generation, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
use nodecraft_types::{CanvasId, EdgeId, Error, GenerationId, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };

/// A text generator stand-in under the real kind id: its plan echoes what reached it.
struct Writer(NodeDescriptor);
impl NodeKind for Writer {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, connected: &[PortType]) -> Option<ModeId> {
        // The mention port never counts as connected: only a wired prompt does.
        ModeId::new(if connected.is_empty() { "write" } else { "rewrite" }).ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let value = Value::List(ctx.inputs.iter().map(|input| input.value.clone()).collect());
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "text".into(), value }])
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let prompt = ParamSpec {
        name: generation::PROMPT.into(),
        label: "Prompt".into(),
        description: None,
        control: ParamControl::Text { multiline: true, max_bytes: 4096 },
        required: false,
        default: None,
        visible_if: None,
    };
    kinds
        .register(Box::new(Writer(NodeDescriptor {
            id: generation::TEXT_KIND.into(),
            label: "Writer".into(),
            category: Category::Generation,
            inputs: vec![Port { name: generation::PROMPT.into(), port_type: TEXT }],
            outputs: vec![Port { name: "text".into(), port_type: TEXT }],
            params: ParamSchema { params: vec![prompt] },
            quick_add: None,
        })))
        .unwrap();
    kinds
}

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn token(n: u8, title: &str) -> String {
    mention::token(title, id(n))
}

fn with_result(mut node: Node, n: u8, value: Value) -> Node {
    let generation = GenerationId::from_bytes([n; 16]);
    node.history.push(Generation { id: generation, outputs: vec![value], input_hash: None, items: Vec::new(), settings: Vec::new(), incomplete: None });
    node.active = Some(generation);
    node
}

/// A writer with `prompt` and a result, so it is used as is unless it is run.
fn writer(n: u8, prompt: &str) -> Node {
    let mut node = kinds().get(generation::TEXT_KIND).unwrap().descriptor().instantiate(id(n), &BTreeMap::new()).unwrap();
    node.params.insert(generation::PROMPT.into(), Value::Text(prompt.into()));
    with_result(node, n, Value::Text(format!("answer {n}")))
}

fn text_source(n: u8, text: &str) -> Node {
    let supplied = [("text".to_owned(), Value::Text(text.into()))].into_iter().collect();
    kinds().get("source.text").unwrap().instantiate_instance(id(n), BTreeMap::new(), &supplied).unwrap()
}

fn image_source(n: u8) -> Node {
    kinds().get("source.image").unwrap().instantiate_instance(id(n), BTreeMap::new(), &Default::default()).unwrap()
}

fn project(nodes: Vec<Node>, wires: &[(u8, &str, u8, &str)]) -> Project {
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Mentions").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for node in nodes {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    for (e, (from, out, to, port)) in wires.iter().enumerate() {
        let edge = Edge {
            id: EdgeId::from_bytes([u8::try_from(e).unwrap().wrapping_add(100); 16]),
            from: Endpoint { node: id(*from), port: (*out).into() },
            to: Endpoint { node: id(*to), port: (*port).into() },
            order: u32::try_from(e).unwrap(),
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    project
}

fn canvas(p: &Project) -> &Canvas {
    p.canvases().get(&C).unwrap()
}

fn planned(p: &Project, n: u8, stale: &[u8]) -> Result<Planned, Error> {
    plan(canvas(p), &kinds(), &RunMode::WithUpstream, &[id(n)].into_iter().collect(), &stale.iter().map(|n| id(*n)).collect())
}

fn mentions(p: &Project, n: u8) -> Vec<(String, Result<String, Problem>)> {
    let incoming = crate::routing::incoming(canvas(p), &kinds()).unwrap();
    of(canvas(p), canvas(p).nodes().get(&id(n)).unwrap(), &incoming.mention_loops)
        .into_iter()
        .map(|found| (found.title.to_owned(), found.resolved.map(str::to_owned)))
        .collect()
}

fn blocked_because(p: &Project, n: u8, stale: &[u8]) -> String {
    let Planned::Blocked(blockers) = planned(p, n, stale).unwrap() else { panic!("planned") };
    let [blocker] = blockers.as_slice() else { panic!("{blockers:?}") };
    assert_eq!(blocker.node, id(n));
    let Reason::Invalid(message) = &blocker.reason else { panic!("{blocker:?}") };
    message.clone()
}

#[test]
fn mentioned_cards_feed_their_text_in_prompt_order_and_run_first() {
    let prompt = format!("Combine {} with {}, then {} again", token(2, "Style"), token(1, "Story"), token(2, "Style"));
    let p = project(vec![text_source(1, "a tale"), writer(2, "style notes"), writer(3, &prompt)], &[]);
    assert_eq!(mentions(&p, 3), vec![("Style".into(), Ok("text".into())), ("Story".into(), Ok("out".into()))]);
    // 2 is stale, so it runs before 3; 1 is a source and lends its text.
    let Planned::Ready(run) = planned(&p, 3, &[2, 3]).unwrap() else { panic!("blocked") };
    let steps: Vec<NodeId> = run.steps.iter().map(|step| step.node).collect();
    assert_eq!(steps, vec![id(2), id(3)]);
    let consumer = run.steps.last().unwrap();
    let input = consumer.inputs.iter().find(|input| input.port == mention::PORT).unwrap();
    assert_eq!(input.from, vec![InputSource::Step { node: id(2), port: "text".into(), list: false }, InputSource::Value(Value::Text("a tale".into()))]);
    assert_eq!(input.origins, vec![id(2), id(1)]);
}

#[test]
fn a_wired_prompt_feeds_no_mentions() {
    let prompt = format!("about {}", token(9, "Gone"));
    let p = project(vec![text_source(1, "wired"), writer(3, &prompt)], &[(1, "out", 3, generation::PROMPT)]);
    assert!(mentions(&p, 3).is_empty());
    assert!(matches!(planned(&p, 3, &[3]).unwrap(), Planned::Ready(_)));
}

#[test]
fn a_missing_or_non_text_card_blocks_the_run() {
    let gone = project(vec![writer(3, &format!("about {}", token(9, "Gone")))], &[]);
    assert_eq!(mentions(&gone, 3), vec![("Gone".into(), Err(Problem::Missing))]);
    assert!(blocked_because(&gone, 3, &[3]).contains("@Gone, which is no longer on this canvas"));

    let picture = project(vec![image_source(1), writer(3, &format!("about {}", token(1, "Photo")))], &[]);
    assert_eq!(mentions(&picture, 3), vec![("Photo".into(), Err(Problem::NotText))]);
    assert!(blocked_because(&picture, 3, &[3]).contains("@Photo, which makes no single text"));
}

#[test]
fn a_mention_of_a_card_made_from_this_one_is_a_loop_not_a_hang() {
    // 3 feeds 4 by wire; 3's prompt mentions 4, and 5 mentions itself.
    let p = project(
        vec![writer(3, &format!("from {}", token(4, "Later"))), writer(4, ""), writer(5, &format!("me: {}", token(5, "Me")))],
        &[(3, "text", 4, generation::PROMPT)],
    );
    assert_eq!(mentions(&p, 3), vec![("Later".into(), Err(Problem::Loop))]);
    assert!(blocked_because(&p, 3, &[3]).contains("@Later, which is made from this card"));
    assert_eq!(mentions(&p, 5), vec![("Me".into(), Err(Problem::Loop))]);
    assert!(blocked_because(&p, 5, &[5]).contains("loop"));
    // Two cards mentioning each other are both loops.
    let pair = project(vec![writer(3, &token(4, "B")), writer(4, &token(3, "A"))], &[]);
    assert_eq!(mentions(&pair, 3), vec![("B".into(), Err(Problem::Loop))]);
    assert_eq!(mentions(&pair, 4), vec![("A".into(), Err(Problem::Loop))]);
    let _ = crate::staleness::stale(canvas(&pair), &kinds());
}

#[test]
fn editing_a_mentioned_card_makes_its_mentioner_stale() {
    let p = project(vec![text_source(1, "a tale"), writer(3, &format!("retell {}", token(1, "Story")))], &[]);
    let before = crate::staleness::input_hash(canvas(&p), &kinds(), id(3)).unwrap();
    let mut changed = p.clone();
    Change::SetNode { canvas: C, node: text_source(1, "another tale") }.apply(&mut changed).unwrap();
    assert_ne!(crate::staleness::input_hash(canvas(&changed), &kinds(), id(3)).unwrap(), before);
    // Without mentions the hash is what it was before mentions existed: no extra port is hashed.
    let plain = project(vec![text_source(1, "a tale"), writer(3, "retell")], &[]);
    let plain_changed = project(vec![text_source(1, "another tale"), writer(3, "retell")], &[]);
    assert_eq!(
        crate::staleness::input_hash(canvas(&plain), &kinds(), id(3)).unwrap(),
        crate::staleness::input_hash(canvas(&plain_changed), &kinds(), id(3)).unwrap()
    );
}
