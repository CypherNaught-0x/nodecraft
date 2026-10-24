//! Exercise real Timeline planning/run assembly; only the native renderer is replaced by a
//! successful output event. The source recipe must be the one handed to that renderer.
use super::tests::{C, RUN, id, kinds, opts};
use super::*;
use crate::planner::{Planned, RunMode, plan};
use nodecraft_graph::{Changeset, Edge, Endpoint, Project};
use nodecraft_types::{AssetRef, Blake3, EdgeId, MediaMeta, ProjectId};

fn video(n: u8) -> Value {
    Value::Media(AssetRef {
        hash: Blake3::from_bytes([n; 32]),
        mime: "video/webm".parse().unwrap(),
        meta: MediaMeta { duration_ms: Some(2000), ..Default::default() },
    })
}

fn project() -> Project {
    let kinds = kinds();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Timeline sources").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Timeline") }.apply(&mut p).unwrap();
    let source = kinds.get("source.video").unwrap().descriptor().instantiate(id(1), &[("asset".into(), video(1))].into()).unwrap();
    let timeline = kinds.get(nodecraft_nodes::timeline::ID).unwrap().descriptor().instantiate(id(2), &Default::default()).unwrap();
    for node in [source, timeline] {
        Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
    }
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([3; 16]),
            from: Endpoint { node: id(1), port: "out".into() },
            to: Endpoint { node: id(2), port: "clips".into() },
            order: 0,
        },
    }
    .apply(&mut p)
    .unwrap();
    p
}

fn start(p: &Project) -> (Run, Progress) {
    let canvas = p.canvases().get(&C).unwrap();
    let Planned::Ready(plan) = plan(canvas, &kinds(), &RunMode::Selected, &BTreeSet::from([id(2)]), &BTreeSet::new()).unwrap() else { panic!("blocked") };
    let mut run_id = RUN;
    run_id[0] ^= u8::try_from(canvas.nodes().get(&id(2)).unwrap().history.len()).unwrap();
    Run::start(&plan, C, canvas, &kinds(), opts(run_id, 1, RunLimits::default())).unwrap()
}

fn finish(run: &mut Run, first: Progress) -> Generation {
    let [job] = first.jobs.as_slice() else { panic!("one Timeline job") };
    let result = run.job_done(&kinds(), job.id, vec![("video".into(), video(3))]);
    assert!(result.failed.is_empty(), "{:?}", result.failed);
    let [step] = result.finished.as_slice() else { panic!("one result") };
    step.generation.clone()
}

#[test]
fn timeline_sources_stamp_actual_recipe_and_edits_stay_stale_until_rerendered() {
    let mut p = project();
    let (mut run, first) = start(&p);
    let generation = finish(&mut run, first);
    let [source] = generation.source_versions.as_slice() else { panic!("clip source") };
    assert_eq!(
        (source.role.clone(), source.hash, source.item),
        (nodecraft_nodes::timeline::auto_id(nodecraft_nodes::timeline::Input::Clips, &id(1).to_string(), 0), Blake3::from_bytes([1; 32]), None)
    );
    let node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap();
    let record = Changeset::new(record_changes(C, node, generation.clone()).unwrap());
    record.apply(&mut p).unwrap();
    let stale = |p: &Project| crate::staleness::stale(p.canvases().get(&C).unwrap(), &kinds()).unwrap();
    assert!(!stale(&p).contains(&id(2)));
    let spec = format!(r#"{{"clips":[{{"id":"clip","origin":"{}","out_us":1500000}}]}}"#, id(1));
    let mut edited = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    edited.config.insert(nodecraft_nodes::timeline::CONFIG.into(), Value::Text(spec));
    let undo = Change::SetNode { canvas: C, node: edited }.apply(&mut p).unwrap();
    assert!(stale(&p).contains(&id(2)), "ordinary stale badge sees Timeline edits");
    undo.apply(&mut p).unwrap();
    assert!(!stale(&p).contains(&id(2)), "undo restores freshness");
    let (mut old_run, old_first) = start(&p);
    Change::SetParam { canvas: C, id: id(1), name: "asset".into(), value: Some(video(2)) }.apply(&mut p).unwrap();
    let old_result = finish(&mut old_run, old_first);
    assert_eq!(old_result.source_versions, generation.source_versions, "in-flight sources are frozen at planning");
    assert!(stale(&p).contains(&id(2)));
    let (mut run, first) = start(&p);
    let updated = finish(&mut run, first);
    assert_eq!(updated.source_versions[0].hash, Blake3::from_bytes([2; 32]));
    let node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap();
    Changeset::new(record_changes(C, node, updated).unwrap()).apply(&mut p).unwrap();
    assert!(!stale(&p).contains(&id(2)), "successful rerender clears existing badge");
}
