use super::*;
use crate::planner::{InputSource, Planned, Reason, RunMode, plan};
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Generation, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry, router};
use nodecraft_types::{
    AssetRef, Blake3, CanvasId, EdgeId, Error, GenerationId, MediaMeta, ParamControl, ParamSchema, ParamSpec, ParamValues, PortType, ProjectId,
};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const IMAGES: PortType = PortType { modality: Modality::Image, many: true };

/// A generator stand-in: a prompt, an image list input, an image list output. Its plan echoes
/// what reached the image input.
struct Gen(NodeDescriptor);
impl NodeKind for Gen {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("gen").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let value = Value::List(ctx.inputs.iter().map(|input| input.value.clone()).collect());
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value }])
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let prompt = ParamSpec {
        name: PROMPT.into(),
        label: "Prompt".into(),
        description: None,
        control: ParamControl::Text { multiline: true, max_bytes: 4096 },
        required: false,
        default: None,
        visible_if: None,
    };
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.gen".into(),
            label: "Gen".into(),
            category: Category::Generation,
            inputs: vec![
                Port { name: "image".into(), port_type: IMAGES },
                Port { name: PROMPT.into(), port_type: PortType { modality: Modality::Text, many: false } },
            ],
            outputs: vec![Port { name: "out".into(), port_type: IMAGES }],
            params: ParamSchema { params: vec![prompt] },
            quick_add: None,
        })))
        .unwrap();
    kinds
}

fn mentioned(p: &Project, n: u8) -> Vec<Mention> {
    of(canvas(p), &kinds(), canvas(p).nodes().get(&id(n)).unwrap())
}

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn image(n: u8) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() })
}

fn with_result(mut node: Node, n: u8, value: Value) -> Node {
    let generation = GenerationId::from_bytes([n; 16]);
    node.history.push(Generation { id: generation, outputs: vec![value], input_hash: None, items: Vec::new(), settings: Vec::new(), incomplete: None });
    node.active = Some(generation);
    node
}

/// A generator with `prompt` and a result, so it is used as is unless it is run.
fn generator(n: u8, prompt: &str) -> Node {
    let mut node = kinds().get("test.gen").unwrap().descriptor().instantiate(id(n), &BTreeMap::new()).unwrap();
    node.params.insert(PROMPT.into(), Value::Text(prompt.into()));
    with_result(node, n, Value::List(vec![image(n)]))
}

fn element(n: u8, name: &str) -> Node {
    let mut supplied = ParamValues::new();
    supplied.insert(element::NAME.into(), Value::Text(name.into()));
    kinds().get(element::ID).unwrap().instantiate_instance(id(n), BTreeMap::new(), &supplied).unwrap()
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

fn planned(p: &Project, n: u8) -> Result<Planned, Error> {
    plan(canvas(p), &kinds(), &RunMode::WithUpstream, &[id(n)].into_iter().collect(), &BTreeSet::new())
}

/// Two pictures (1, 2) feed Element 3 "Hero"; generator 4 has its own picture wired, generator 5
/// mentions it.
fn scene(prompt: &str) -> Project {
    project(
        vec![generator(1, ""), generator(2, ""), element(3, "Hero"), generator(4, ""), generator(5, prompt)],
        &[(1, "out", 3, element::IMAGES), (2, "out", 3, element::IMAGES), (4, "out", 5, "image")],
    )
}

#[test]
fn a_mention_feeds_the_elements_references_after_the_wires() {
    let p = scene("the #hero, in red");
    let Planned::Ready(run) = planned(&p, 5).unwrap() else { panic!("blocked") };
    let steps: Vec<NodeId> = run.steps.iter().map(|step| step.node).collect();
    // The Element runs first (it has no result yet), then the generator.
    assert_eq!(steps, vec![id(3), id(5)]);
    let element_step = run.steps.first().unwrap();
    assert_eq!(element_step.jobs.as_ref().unwrap().len(), 2, "one source job per reference image");
    let consumer = run.steps.get(1).unwrap();
    let input = consumer.inputs.iter().find(|input| input.port == "image").unwrap();
    assert_eq!(input.from, vec![InputSource::Value(image(4)), InputSource::Step { node: id(3), port: element::REFERENCE.into(), list: true }]);
    assert_eq!(mentioned(&p, 5), vec![Mention { name: "hero".into(), resolved: Resolved::Element(id(3)) }]);
}

#[test]
fn unknown_tags_stay_text_and_block_nothing() {
    let p = scene("colour #ff0000, shot #1");
    let Planned::Ready(run) = planned(&p, 5).unwrap() else { panic!("blocked") };
    assert_eq!(run.steps.len(), 1);
    let resolved: Vec<Resolved> = mentioned(&p, 5).into_iter().map(|m| m.resolved).collect();
    assert_eq!(resolved, vec![Resolved::Unknown, Resolved::Unknown]);
}

#[test]
fn a_shared_name_blocks_the_mentioning_node() {
    let mut p = scene("#Hero");
    Change::AddNode { canvas: C, node: element(6, "HERO") }.apply(&mut p).unwrap();
    let Planned::Blocked(blockers) = planned(&p, 5).unwrap() else { panic!("planned") };
    assert!(
        matches!(blockers.as_slice(), [b] if b.node == id(5) && matches!(&b.reason, Reason::Invalid(m) if m.contains("more than one Element"))),
        "{blockers:?}"
    );
    assert_eq!(mentioned(&p, 5).first().unwrap().resolved, Resolved::Ambiguous);
}

#[test]
fn a_wired_element_is_not_fed_twice() {
    let mut p = scene("#hero");
    let edge = Edge {
        id: EdgeId::from_bytes([200; 16]),
        from: Endpoint { node: id(3), port: element::REFERENCE.into() },
        to: Endpoint { node: id(5), port: "image".into() },
        order: 9,
    };
    Change::Connect { canvas: C, edge }.apply(&mut p).unwrap();
    let Planned::Ready(run) = planned(&p, 5).unwrap() else { panic!("blocked") };
    let consumer = run.steps.iter().find(|step| step.node == id(5)).unwrap();
    assert_eq!(consumer.inputs.iter().find(|input| input.port == "image").unwrap().from.len(), 2);
    assert_eq!(mentioned(&p, 5).first().unwrap().resolved, Resolved::Wired(id(3)));
}

#[test]
fn a_mention_loop_is_an_error_not_a_hang() {
    // 5 feeds Element 3, whose references 5's prompt mentions.
    let p = project(vec![element(3, "hero"), generator(5, "#hero")], &[(5, "out", 3, element::IMAGES)]);
    let mut stale = BTreeSet::new();
    stale.insert(id(5));
    let result = plan(canvas(&p), &kinds(), &RunMode::WithUpstream, &[id(5)].into_iter().collect(), &stale);
    assert!(result.unwrap_err().message.contains("loop"));
    // Staleness hashing must terminate too.
    let _ = crate::staleness::stale(canvas(&p), &kinds());
}

#[test]
fn editing_an_element_makes_its_mentioners_stale() {
    let p = scene("#hero");
    let before = crate::staleness::input_hash(canvas(&p), &kinds(), id(5)).unwrap();
    let mut changed = p.clone();
    let mut node = canvas(&p).nodes().get(&id(3)).unwrap().clone();
    node = with_result(node, 3, image(7));
    Change::SetNode { canvas: C, node }.apply(&mut changed).unwrap();
    assert_ne!(crate::staleness::input_hash(canvas(&changed), &kinds(), id(5)).unwrap(), before);
    // A prompt without the mention doesn't depend on the Element.
    let plain = scene("hero");
    let mut plain_changed = plain.clone();
    let node = with_result(canvas(&plain).nodes().get(&id(3)).unwrap().clone(), 3, image(7));
    Change::SetNode { canvas: C, node }.apply(&mut plain_changed).unwrap();
    assert_eq!(
        crate::staleness::input_hash(canvas(&plain), &kinds(), id(5)).unwrap(),
        crate::staleness::input_hash(canvas(&plain_changed), &kinds(), id(5)).unwrap()
    );
}

fn image_feeds(p: &Project, n: u8) -> Vec<InputSource> {
    let Planned::Ready(run) = planned(p, n).unwrap() else { panic!("blocked") };
    let consumer = run.steps.iter().find(|step| step.node == id(n)).unwrap();
    consumer.inputs.iter().find(|input| input.port == "image").map(|input| input.from.clone()).unwrap_or_default()
}

#[test]
fn an_unrun_sheet_card_is_not_run_and_the_element_uses_its_images() {
    let sheet = kinds().get("test.gen").unwrap().descriptor().instantiate(id(6), &BTreeMap::new()).unwrap();
    let mut p = scene("#hero");
    Change::AddNode { canvas: C, node: sheet }.apply(&mut p).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([201; 16]),
        from: Endpoint { node: id(6), port: "out".into() },
        to: Endpoint { node: id(3), port: element::SHEET.into() },
        order: 0,
    };
    Change::Connect { canvas: C, edge }.apply(&mut p).unwrap();
    let Planned::Ready(run) = planned(&p, 5).unwrap() else { panic!("blocked") };
    let steps: Vec<NodeId> = run.steps.iter().map(|step| step.node).collect();
    assert_eq!(steps, vec![id(3), id(5)], "the paid sheet run isn't pulled in");
    assert_eq!(run.steps.first().unwrap().jobs.as_ref().unwrap().len(), 2, "the Element gives its images");
    // Once the sheet has a result, the Element gives it instead.
    let ran = with_result(canvas(&p).nodes().get(&id(6)).unwrap().clone(), 6, Value::List(vec![image(6)]));
    Change::SetNode { canvas: C, node: ran }.apply(&mut p).unwrap();
    let Planned::Ready(run) = planned(&p, 5).unwrap() else { panic!("blocked") };
    assert_eq!(run.steps.first().unwrap().jobs.as_ref().unwrap().len(), 1, "the sheet replaces the images");
}

#[test]
fn a_wired_prompt_mentions_nothing() {
    let mut p = scene("#hero");
    Change::AddNode { canvas: C, node: kinds().get("source.text").unwrap().descriptor().instantiate(id(7), &BTreeMap::new()).unwrap() }.apply(&mut p).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([202; 16]),
        from: Endpoint { node: id(7), port: "out".into() },
        to: Endpoint { node: id(5), port: PROMPT.into() },
        order: 0,
    };
    Change::Connect { canvas: C, edge }.apply(&mut p).unwrap();
    assert!(mentioned(&p, 5).is_empty());
    assert_eq!(crate::mentions::feeds(canvas(&p), &kinds()).feeds.len(), 0);
}

#[test]
fn an_element_routed_in_is_not_fed_twice() {
    let mut p = scene("#hero");
    let router = kinds().get(router::ID).unwrap().instantiate_instance(id(8), router::config(Modality::Image), &ParamValues::new()).unwrap();
    Change::AddNode { canvas: C, node: router }.apply(&mut p).unwrap();
    for (n, from, to) in [
        (203, Endpoint { node: id(3), port: element::REFERENCE.into() }, Endpoint { node: id(8), port: "in".into() }),
        (204, Endpoint { node: id(8), port: "out".into() }, Endpoint { node: id(5), port: "image".into() }),
    ] {
        Change::Connect { canvas: C, edge: Edge { id: EdgeId::from_bytes([n; 16]), from, to, order: 5 } }.apply(&mut p).unwrap();
    }
    let element_feeds = image_feeds(&p, 5).into_iter().filter(|source| matches!(source, InputSource::Step { node, .. } if *node == id(3))).count();
    assert_eq!(element_feeds, 1);
}

#[test]
fn renaming_or_deleting_the_element_drops_the_mention_and_stales_the_card() {
    let p = scene("#hero");
    let before = crate::staleness::input_hash(canvas(&p), &kinds(), id(5)).unwrap();
    let mut renamed = p.clone();
    let mut node = canvas(&p).nodes().get(&id(3)).unwrap().clone();
    node.params.insert(element::NAME.into(), Value::Text("villain".into()));
    Change::SetNode { canvas: C, node }.apply(&mut renamed).unwrap();
    assert_eq!(mentioned(&renamed, 5).first().unwrap().resolved, Resolved::Unknown);
    assert_ne!(crate::staleness::input_hash(canvas(&renamed), &kinds(), id(5)).unwrap(), before);
    let mut deleted = p.clone();
    let edges: Vec<EdgeId> = canvas(&p).edges().values().filter(|e| e.to.node == id(3)).map(|e| e.id).collect();
    for edge in edges {
        Change::Disconnect { canvas: C, id: edge }.apply(&mut deleted).unwrap();
    }
    Change::RemoveNode { canvas: C, id: id(3) }.apply(&mut deleted).unwrap();
    assert_eq!(mentioned(&deleted, 5).first().unwrap().resolved, Resolved::Unknown);
    assert_ne!(crate::staleness::input_hash(canvas(&deleted), &kinds(), id(5)).unwrap(), before);
}
