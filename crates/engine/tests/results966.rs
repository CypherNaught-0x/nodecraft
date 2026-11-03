//! Ordinary collection-producing actions must survive the real planner/Run assembly path.
#![cfg(unix)]
use nodecraft_engine::{Session, action_exec::LocalActions, generation_catalog::GenerationCatalog, templates};
use nodecraft_exec::{
    planner::{Planned, RunMode},
    run::{ActionExecutor, Run, RunOptions},
    scheduler::RetryPolicy,
};
use nodecraft_format::{MemoryStorage, Snapshot};
use nodecraft_graph::Changeset;
use nodecraft_nodes::JobSpec;
use nodecraft_types::{AssetRef, ProviderTarget, TargetSelection, Value};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Held(Mutex<Option<nodecraft_engine::save_runner::SaveJob>>);
impl nodecraft_engine::save_runner::SaveRunner for Held {
    fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}
impl Held {
    fn run(&self) {
        self.0.lock().unwrap().take().unwrap().run();
    }
}

fn ordinary_action(kind: &str) -> (Snapshot, nodecraft_types::CanvasId, nodecraft_types::NodeId) {
    let entry = templates::entries().unwrap().into_iter().find(|entry| entry.id == format!("action-{}", kind.replace('.', "-"))).unwrap();
    let mut snapshot = templates::load(&entry.id).unwrap();
    let catalog = GenerationCatalog::builtin().unwrap();
    let kinds = catalog.registry().unwrap();
    let canvas = snapshot.project.canvases().get(&entry.canvas).unwrap();
    let id = canvas.nodes().values().find(|node| node.kind == kind).unwrap().id;
    let expanded = nodecraft_exec::technique::expand(snapshot.project.canvases(), canvas, &kinds).unwrap();
    let mut targets = TargetSelection::empty();
    for node in expanded.canvas.nodes().values() {
        for model in catalog.run_catalog().catalog().models() {
            if targets.get(node.id, &model.provider).is_none() {
                targets.insert(node.id, &ProviderTarget::default_instance(&model.provider).unwrap()).unwrap();
            }
        }
    }
    let stale = nodecraft_exec::staleness::stale(&expanded.canvas, &kinds).unwrap();
    let requested = [id].into_iter().collect();
    let Planned::Ready(plan) =
        nodecraft_exec::planner::plan_expanded(&expanded, &kinds, &RunMode::WithUpstream, &requested, &stale, &targets, &nodecraft_types::NoOptions).unwrap()
    else {
        panic!("ready collection action")
    };
    let (admitted, _) = nodecraft_exec::admission::admit_run_with_targets(
        plan,
        entry.canvas,
        expanded.canvas.clone().into_owned(),
        &kinds,
        catalog.run_catalog().catalog(),
        Default::default(),
        &targets,
    )
    .unwrap();
    let (mut run, first) =
        Run::start_admitted(admitted, &kinds, RunOptions { id: [96; 16], first_job: 1, limits: Default::default(), retry: RetryPolicy::NEVER }).unwrap();
    let mut queue = VecDeque::from(first.jobs);
    let mut finished = first.finished;
    assert!(first.failed.is_empty());
    while let Some(job) = queue.pop_front() {
        let outputs = match run.work(job.id).unwrap().clone() {
            JobSpec::Source { output, value, .. } => vec![(output, value)],
            JobSpec::Action { action, params, inputs, output, .. } => {
                let value = if action == "image.duplicate" {
                    // Real CPU action. No provider or native process is involved.
                    let actions = LocalActions::new(snapshot.assets.clone());
                    let value = actions.execute(&action, &params, &inputs).unwrap();
                    for (_, bytes) in actions.staged().iter() {
                        snapshot.assets.insert(bytes).unwrap();
                    }
                    value
                } else {
                    assert!(matches!(action.as_str(), "video.frames" | "mesh.views"));
                    // Explicit mock native-resource output; real planning/Run still assembles
                    // the ordinary many-port result. Native codecs/GPU have separate tests.
                    let bytes = nodecraft_media::encode_png(2, 2, &[255; 16], &Default::default()).unwrap();
                    let hash = snapshot.assets.insert(&bytes).unwrap();
                    let image = Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() });
                    Value::List(vec![image.clone(), image])
                };
                vec![(output, value)]
            }
            JobSpec::Generate { .. } => panic!("collection regression must not call a provider"),
        };
        let progress = run.job_done(&kinds, job.id, outputs);
        assert!(progress.failed.is_empty(), "{:?}", progress.failed);
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
    }
    assert!(run.is_finished());
    for result in finished {
        let node = snapshot.project.canvases().get(&entry.canvas).unwrap().nodes().get(&result.node).unwrap();
        let changes = nodecraft_exec::run::record_changes(entry.canvas, node, result.generation).unwrap();
        Changeset::new(changes).apply(&mut snapshot.project).unwrap();
    }
    (snapshot, entry.canvas, id)
}

#[test]
fn results966_real_run_ordinary_frames_views_and_duplicate_export_every_nested_media() {
    for kind in ["video.frames", "mesh.views", "image.duplicate"] {
        let (snapshot, canvas, id) = ordinary_action(kind);
        let node = snapshot.project.canvases().get(&canvas).unwrap().nodes().get(&id).unwrap();
        let generation = node.history.last().unwrap();
        assert!(generation.items.is_empty(), "ordinary action, no fan tags");
        assert!(matches!(generation.outputs.as_slice(), [Value::List(_)]));
        if kind == "video.frames" {
            assert!(
                matches!(generation.outputs.as_slice(),[Value::List(outer)] if matches!(outer.first(),Some(Value::List(_)))),
                "the real Run path must exercise the previously omitted nested result"
            );
        }
        let port = node.outputs[0].name.clone();
        let gid = generation.id;
        let leaves = nodecraft_engine::export_leaves::iter(&generation.outputs[0], false, &mut Default::default()).collect::<Result<Vec<_>, _>>().unwrap();
        let expected = leaves
            .iter()
            .map(|leaf| {
                let Value::Media(asset) = leaf.value else { panic!("media") };
                (leaf.item, asset.hash)
            })
            .collect::<Vec<_>>();
        assert!(expected.len() >= 2, "collection must exercise multiple media");
        let mut session = Session::with_storage(snapshot, Box::new(MemoryStorage::default())).unwrap();
        let runner = Arc::new(Held::default());
        session.set_export_runner(runner.clone());
        let dir = tempfile::tempdir().unwrap();
        let host = nodecraft_engine::local_export::ExportHost::authorize(dir.path()).unwrap();
        let args = json!({"canvas":canvas,"nodes":[id],"destination":dir.path()});
        let plan = nodecraft_engine::local_export::dispatch_with_host(&mut session, &host, "export.results_plan", &args).unwrap();
        assert_eq!(plan["total"], expected.len(), "{kind}");
        nodecraft_engine::local_export::dispatch_with_host(&mut session, &host, "export.results_start", &args).unwrap();
        runner.run();
        let Some(nodecraft_engine::export_job::ExportResult::Results { report }) = session.export_status().unwrap().result else { panic!("batch receipt") };
        assert_eq!(report.written(), expected.len());
        assert_eq!(report.failed(), 0);
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join(report.manifest.unwrap())).unwrap()).unwrap();
        for (entry, (item, hash)) in manifest["entries"].as_array().unwrap().iter().zip(&expected) {
            assert_eq!(entry["result"]["item"], json!(item));
            assert_eq!(entry["result"]["hash"], json!(hash));
            let bytes = std::fs::read(dir.path().join(entry["result"]["file"].as_str().unwrap())).unwrap();
            assert_eq!(nodecraft_types::Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()), *hash);
        }
        let path = dir.path().join("chosen.png");
        session
            .dispatch(
                "history.export",
                &json!({"canvas":canvas,"id":id,"generation":gid,"port":port,"item":expected.last().unwrap().0,"path":path,"background":true}),
            )
            .unwrap();
        runner.run();
        assert!(path.exists());
    }
}
