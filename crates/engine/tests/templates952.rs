//! Every shipped starter is opened, verified, planned and run offline; native resources have an explicit second profile.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::{Session, action_exec::LocalActions, generation_catalog::GenerationCatalog, templates};
use nodecraft_exec::{
    planner::{Planned, RunMode},
    run::{ActionExecutor, Run, RunOptions},
    scheduler::RetryPolicy,
};
use nodecraft_format::{AssetStore, Snapshot};
use nodecraft_graph::{Canvas, Change, Changeset, Project};
use nodecraft_nodes::JobSpec;
use nodecraft_providers::{
    Completed, Output, Provider, Submission,
    mock::{MockModel, MockOutput, MockProvider},
};
use nodecraft_types::{AssetRef, CanvasId, Modality, ProjectId, ProviderTarget, TargetSelection, Value};
use serde_json::json;
use std::{
    collections::{BTreeSet, VecDeque},
    path::PathBuf,
};

fn image() -> Vec<u8> {
    nodecraft_media::encode_png(64, 64, &[150; 64 * 64 * 4], &Default::default()).unwrap()
}
fn fixture(modality: Modality) -> (&'static str, Vec<u8>) {
    match modality {
        Modality::Image => ("image/png", image()),
        Modality::Video => ("video/mp4", include_bytes!("../../../assets/templates/fixtures/blue-tone.mp4").to_vec()),
        Modality::Audio => ("audio/wav", include_bytes!("../../providers/src/fal/fixtures/tone.wav").to_vec()),
        Modality::Mesh => ("model/gltf-binary", include_bytes!("../../providers/src/fal/fixtures/triangle.glb").to_vec()),
        _ => panic!("a media fixture must name a media modality"),
    }
}
fn media(assets: &mut AssetStore, modality: Modality) -> Value {
    let (mime, bytes) = fixture(modality);
    let hash = assets.insert(&bytes).unwrap();
    Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: Default::default() })
}
fn native(action: &str) -> bool {
    action == "image.shader" || action.starts_with("video.") || matches!(action, "mesh.photo" | "mesh.views" | "mesh.turntable")
}
fn run(entry: &templates::Entry, real_native: bool) -> Snapshot {
    let mut snapshot = templates::load(&entry.id).unwrap();
    let generation = GenerationCatalog::builtin().unwrap();
    let kinds = generation.registry().unwrap();
    let catalog = generation.run_catalog().catalog().clone();
    let canvas = snapshot.project.canvases().get(&entry.canvas).unwrap();
    let expanded = nodecraft_exec::technique::expand(snapshot.project.canvases(), canvas, &kinds).unwrap();
    assert!(expanded.blockers.is_empty(), "{}: {:?}", entry.id, expanded.blockers);
    let mut targets = TargetSelection::empty();
    for node in expanded.canvas.nodes().values() {
        for model in catalog.models() {
            if targets.get(node.id, &model.provider).is_none() {
                targets.insert(node.id, &ProviderTarget::default_instance(&model.provider).unwrap()).unwrap();
            }
        }
    }
    let stale = nodecraft_exec::staleness::stale(&expanded.canvas, &kinds).unwrap();
    let requested = canvas.nodes().keys().copied().collect();
    let planned =
        nodecraft_exec::planner::plan_expanded(&expanded, &kinds, &RunMode::WithUpstream, &requested, &stale, &targets, &nodecraft_types::NoOptions).unwrap();
    let Planned::Ready(plan) = planned else { panic!("{} is blocked: {planned:?}", entry.id) };
    let (admitted, _) = nodecraft_exec::admission::admit_run_with_targets(
        plan,
        entry.canvas,
        expanded.canvas.clone().into_owned(),
        &kinds,
        &catalog,
        Default::default(),
        &targets,
    )
    .unwrap();
    let (mut run, first) =
        Run::start_admitted(admitted, &kinds, RunOptions { id: [95; 16], first_job: 1, limits: Default::default(), retry: RetryPolicy::NEVER }).unwrap();
    let mut queue = VecDeque::from(first.jobs);
    let mut finished = first.finished;
    let mut failures = first.failed;
    let mut calls = 0usize;
    while let Some(job) = queue.pop_front() {
        let spec = run.work(job.id).unwrap().clone();
        let outputs = match spec {
            JobSpec::Source { output, value, .. } => vec![(output, value)],
            JobSpec::Action { action, params, inputs, output, .. } => {
                if action == "video.stitch" {
                    assert_eq!(inputs.len(), 2, "the starter joins two ordered clips");
                    assert!(inputs.iter().all(|value| matches!(value, Value::Media(_))));
                }
                let value = if native(&action) && !real_native {
                    // An explicit offline resource profile, not a claim to execute FFmpeg or a GPU.
                    // Real native semantics are exercised by templates952_native_resources below.
                    let d = kinds.get(&action).unwrap().descriptor().outputs.first().unwrap().clone();
                    let value = media(&mut snapshot.assets, d.port_type.modality);
                    if d.port_type.many { Value::List(vec![value.clone(), value]) } else { value }
                } else {
                    let tools = if real_native {
                        Some((
                            PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap()),
                            PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap()),
                        ))
                    } else {
                        None
                    };
                    let actions = LocalActions::new(snapshot.assets.clone()).with_video_tools(tools);
                    let value = actions.execute(&action, &params, &inputs).unwrap_or_else(|e| panic!("{} / {action}: {e}", entry.id));
                    for (_, bytes) in actions.staged().iter() {
                        snapshot.assets.insert(bytes).unwrap();
                    }
                    for (_, bytes) in actions.staged_videos() {
                        snapshot.assets.insert(&bytes).unwrap();
                    }
                    value
                };
                vec![(output, value)]
            }
            spec @ JobSpec::Generate { .. } => {
                let JobSpec::Generate { ref outputs, .. } = spec else { panic!() };
                let (provider, request) = nodecraft_engine::generate::provider_job_with_media(&spec, &job.idempotency_key, &snapshot.assets).unwrap();
                let modality = outputs.first().unwrap().modality;
                let mock_output = match modality {
                    Modality::Image => MockOutput::Image,
                    Modality::Video => MockOutput::Video,
                    Modality::Audio => MockOutput::Audio,
                    Modality::Style => MockOutput::Style,
                    _ => MockOutput::Text,
                };
                let mock = MockProvider::new(provider.clone(), [(request.model.clone(), MockModel::new(mock_output))]);
                let Submission::Finished(mut done) = futures::executor::block_on(mock.submit(&request)).unwrap() else { panic!() };
                assert_eq!(mock.requests().len(), 1);
                calls += 1;
                // MockProvider's raster/video/audio signatures are deliberately not full codecs.
                // Attach our authored valid files while retaining its exact typed job invocation.
                if modality != Modality::Text {
                    done = if modality == Modality::Style {
                        let style = nodecraft_media::style::Style {
                            provider: provider.clone(),
                            trainer: request.model.clone(),
                            family: outputs[0].style_family.clone().unwrap(),
                            trigger: "paper".into(),
                            weights: "https://fal.media/original-offline-style.safetensors".into(),
                        };
                        Completed::new(vec![Output::Bytes {
                            mime: nodecraft_media::style::MIME.into(),
                            bytes: nodecraft_media::style::encode(&style).unwrap().into(),
                        }])
                    } else {
                        let (mime, bytes) = fixture(modality);
                        Completed::new(vec![Output::Bytes { mime: mime.into(), bytes: bytes.into() }])
                    };
                }
                let produced = nodecraft_engine::generate::produced(outputs, &done).unwrap();
                for (hash, bytes) in produced.assets {
                    assert_eq!(snapshot.assets.insert(&bytes).unwrap(), hash);
                }
                produced.outputs
            }
        };
        let progress = run.job_done(&kinds, job.id, outputs);
        queue.extend(progress.jobs);
        finished.extend(progress.finished);
        failures.extend(progress.failed);
    }
    assert!(run.is_finished(), "{} did not finish", entry.id);
    assert!(failures.is_empty(), "{} failed: {failures:?}", entry.id);
    assert!(!finished.is_empty(), "{} has no executable demonstration", entry.id);
    for result in finished {
        if let Some(node) = snapshot.project.canvases().get(&entry.canvas).unwrap().nodes().get(&result.node) {
            let changes = nodecraft_exec::run::record_changes(entry.canvas, node, result.generation).unwrap();
            Changeset::new(changes).apply(&mut snapshot.project).unwrap();
        }
    }
    match entry.id.as_str() {
        "csv-batch" | "prompt-list" | "folder" => assert_eq!(calls, 2, "one provider call per item"),
        "batch-cross" => assert_eq!(calls, 8, "two + two preparers and four crossed pairs"),
        "batch-zip" => assert_eq!(calls, 6, "two + two preparers and two zipped pairs"),
        "train-style" => assert_eq!(calls, 2, "training followed by applying the compatible style"),
        "generation-sheet" => assert_eq!(calls, 3, "two variations on the first card, one on the second"),
        "action-text-concat" => {
            let canvas = snapshot.project.canvases().get(&entry.canvas).unwrap();
            let result = canvas.nodes().values().find(|n| n.kind == "text.concat").unwrap().history.last().unwrap();
            let Some(Value::Text(joined)) = result.outputs.first() else { panic!("joined text") };
            assert!(joined.starts_with("Paper boat,Paper kite"));
            assert!(joined.contains("A bright paper kite"));
        }
        _ => {}
    }
    snapshot.assets.validate_project(&snapshot.project).unwrap();
    let encoded = nodecraft_format::encode(&snapshot).unwrap();
    let reopened = nodecraft_format::decode(&encoded).unwrap();
    assert_eq!(reopened, snapshot);
    snapshot
}
#[test]
fn templates952_every_archive_validates_and_runs_with_mock_providers_and_explicit_mock_native_resources() {
    for entry in templates::entries().unwrap() {
        run(&entry, false);
    }
}
#[test]
fn templates952_inventory_covers_every_reachable_builtin_node_kind_and_all_named_workflows() {
    let kinds = GenerationCatalog::builtin().unwrap().registry().unwrap();
    let entries = templates::entries().unwrap();
    let mut covered = BTreeSet::new();
    let mut features = BTreeSet::new();
    for entry in &entries {
        let snapshot = templates::load(&entry.id).unwrap();
        for canvas in snapshot.project.canvases().values() {
            for node in canvas.nodes().values() {
                kinds.get(&node.kind).unwrap().validate_instance(node, false).unwrap();
                covered.insert(node.kind.clone());
            }
        }
        features.extend(entry.features.iter().cloned());
        let text = serde_json::to_string(&snapshot.project).unwrap();
        for forbidden in ["/Users/", "C:\\Users\\", "Authorization", "api_key", "Bearer "] {
            assert!(!text.contains(forbidden), "{} contains {forbidden}", entry.id);
        }
        assert!(snapshot.project.canvases().values().flat_map(|c| c.nodes().values()).any(|node| node.kind == "structure.note"));
    }
    for d in kinds.descriptors() {
        assert!(covered.contains(&d.id), "reachable node kind {} needs its own small template", d.id);
    }
    for required in [
        "text-generation",
        "image-generation",
        "video-generation",
        "audio-generation",
        "mesh-generation",
        "generate-document",
        "cloud-local-switch",
        "mapped-comfy-workflow",
        "combine",
        "branch",
        "prompt-helper",
        "storyboard",
        "layout",
        "csv-batch",
        "prompt-list",
        "batch-cross",
        "batch-zip",
        "technique",
        "published-technique",
        "app-form",
        "router",
    ] {
        assert!(features.contains(required), "missing {required}");
    }
}
#[test]
fn templates952_add_is_atomic_undoable_rejects_repeats_and_preserves_unsaved_work_and_assets() {
    let mut project = Project::new(ProjectId::from_bytes([95; 16]), "Unsaved work").unwrap();
    let original = CanvasId::from_bytes([1; 16]);
    Change::AddCanvas { canvas: Canvas::new(original, "My canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let initial = session.document().clone();
    for entry in templates::entries().unwrap() {
        session.dispatch("template.add", &json!({"id":entry.id})).unwrap();
        assert!(session.document().canvases().contains_key(&original));
        let added = session.document().clone();
        session.assets().validate_project(session.document()).unwrap();
        let revision = session.revision();
        assert!(session.dispatch("template.add", &json!({"id":entry.id})).is_err());
        assert_eq!(session.revision(), revision);
        assert_eq!(session.document(), &added);
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document(), &initial);
        session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(session.document(), &added);
        session.assets().validate_project(session.document()).unwrap();
        session.dispatch("edit.undo", &json!({})).unwrap();
    }
    let before = session.document().clone();
    assert!(session.dispatch("template.add", &json!({"id":"../private"})).is_err());
    assert_eq!(session.document(), &before);
}
#[test]
fn templates952_pdf_creation_and_storyboard_export_generate_valid_complete_documents() {
    for id in ["generate-document", "storyboard"] {
        let entry = templates::entries().unwrap().into_iter().find(|e| e.id == id).unwrap();
        let snapshot = run(&entry, false);
        let canvas = snapshot.project.canvases().get(&entry.canvas).unwrap();
        let deck = canvas.nodes().values().find(|n| n.kind == "structure.deck").unwrap().id;
        let mut session = Session::with_storage(snapshot, Box::new(nodecraft_format::MemoryStorage::default())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("original-document.pdf");
        let result = session.dispatch("deck.export", &json!({"canvas":entry.canvas,"id":deck,"format":"pdf","path":path})).unwrap();
        assert_eq!(result["missing"], json!([]));
        let bytes = std::fs::read(path).unwrap();
        let document = nodecraft_media::Document::open(&bytes, &Default::default()).unwrap();
        assert_eq!(document.page_count(), if id == "storyboard" { 3 } else { 1 });
    }
}
fn asset_output(snapshot: &Snapshot, canvas: CanvasId, kind: &str) -> AssetRef {
    let node = snapshot.project.canvases().get(&canvas).unwrap().nodes().values().find(|n| n.kind == kind).unwrap();
    let Some(Value::Media(asset)) = node.history.last().unwrap().outputs.first() else { panic!("one media result") };
    asset.clone()
}
fn pixels(bytes: &[u8]) -> nodecraft_media::image::RgbaImage {
    nodecraft_media::decode_image(bytes, &Default::default()).unwrap().1.to_rgba8()
}
#[test]
fn templates952_original_images_are_opaque_and_asymmetric_and_edits_are_visible() {
    for kind in ["image.rotate", "image.flip", "image.blur", "image.key"] {
        let entry = templates::entries().unwrap().into_iter().find(|e| e.id == format!("action-{}", kind.replace('.', "-"))).unwrap();
        let snapshot = run(&entry, false);
        let canvas = snapshot.project.canvases().get(&entry.canvas).unwrap();
        let source = canvas.nodes().values().find(|n| n.kind == "source.image").unwrap();
        let Some(Value::Media(asset)) = source.params.get("asset") else { panic!("bundled original") };
        let before = pixels(snapshot.assets.get(&asset.hash).unwrap());
        assert!(before.pixels().all(|p| p.0[3] == 255));
        assert!(before.pixels().any(|p| p.0 == [0, 255, 0, 255]), "default key has a green background to remove");
        let output = asset_output(&snapshot, entry.canvas, kind);
        let after = pixels(snapshot.assets.get(&output.hash).unwrap());
        assert_ne!(before.as_raw(), after.as_raw(), "{kind} must visibly change the bundled example");
        if kind == "image.key" {
            assert!(after.pixels().any(|p| p.0[3] == 0), "green becomes transparent");
            assert!(after.pixels().any(|p| p.0[3] == 255), "the foreground remains opaque");
        }
    }
}
fn marker_x(bytes: &[u8]) -> f64 {
    let picture = pixels(bytes);
    let mut sum = 0usize;
    let mut count = 0usize;
    for (x, _, p) in picture.enumerate_pixels() {
        if p.0[0] > 160 && (91..210).contains(&p.0[1]) && p.0[2] < 130 {
            sum += x as usize;
            count += 1;
        }
    }
    assert!(count > 0, "authored orange moving marker is visible after decoding");
    sum as f64 / count as f64
}
fn marker_frames(snapshot: &Snapshot, asset: AssetRef, count: u64) -> Vec<f64> {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let kind = kinds.get("video.frames").unwrap();
    let params = [("frames".into(), Value::Unsigned(nodecraft_types::Unsigned(count))), ("size".into(), Value::Unsigned(nodecraft_types::Unsigned(32)))].into();
    let node = kind.instantiate_instance(nodecraft_types::NodeId::from_bytes([95; 16]), Default::default(), &params).unwrap();
    let params = kind.validate_instance(&node, true).unwrap().into_visible_values();
    let actions = LocalActions::new(snapshot.assets.clone()).with_video_tools(Some((
        PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap()),
        PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap()),
    )));
    let Value::List(frames) = actions.execute("video.frames", &params, &[Value::Media(asset)]).unwrap() else { panic!("decoded frames") };
    let staged = actions.staged();
    frames
        .iter()
        .map(|value| {
            let Value::Media(frame) = value else { panic!("PNG frame") };
            marker_x(staged.get(&frame.hash).unwrap())
        })
        .collect()
}
#[test]
#[ignore = "native offline profile: configured FFmpeg/ffprobe and GPU; no network or paid calls"]
fn templates952_native_resources_execute_the_real_video_and_gpu_actions() {
    for entry in templates::entries().unwrap() {
        if entry.features.iter().any(|f| native(f) || f == "timeline") {
            let snapshot = run(&entry, true);
            if entry.id == "action-video-frames" {
                let node = snapshot.project.canvases().get(&entry.canvas).unwrap().nodes().values().find(|n| n.kind == "video.frames").unwrap();
                let Some(Value::List(frames)) = node.history.last().unwrap().outputs.first() else { panic!("frame results") };
                let positions: Vec<_> = nodecraft_engine::history_cmds::list_items(frames, true)
                    .map(|value| {
                        let Value::Media(frame) = value else { panic!("frame") };
                        marker_x(snapshot.assets.get(&frame.hash).unwrap())
                    })
                    .collect();
                assert_eq!(positions.len(), 2);
                assert!(positions[0] < positions[1], "two extracted frames show original rightward motion: {positions:?}");
            }
            if entry.id == "action-video-reverse" {
                let positions = marker_frames(&snapshot, asset_output(&snapshot, entry.canvas, "video.reverse"), 2);
                assert!(positions[0] > positions[1], "reversed frames move left: {positions:?}");
            }
            if entry.id == "action-video-stitch" {
                let asset = asset_output(&snapshot, entry.canvas, "video.stitch");
                assert_eq!(asset.meta.duration_ms, Some(2000), "both one-second clips are retained");
                let positions = marker_frames(&snapshot, asset, 4);
                assert!(positions[0] < positions[1] && positions[2] > positions[3], "original clip precedes its reversed clip: {positions:?}");
            }
        }
    }
}

#[test]
fn templates952_cloud_and_local_switch_keeps_the_same_generation_node_and_wires() {
    let entry = templates::entries().unwrap().into_iter().find(|e| e.id == "cloud-local").unwrap();
    let snapshot = templates::load(&entry.id).unwrap();
    let mut session = Session::new(snapshot.project).unwrap();
    let canvas = session.document().canvases().get(&entry.canvas).unwrap();
    let card = canvas.nodes().values().find(|n| n.kind == "generation.image").unwrap().id;
    let edges = canvas.edges().clone();
    let nodes = canvas.nodes().keys().copied().collect::<Vec<_>>();
    for model in ["comfyui.sdxl", "fal.flux-schnell"] {
        let preview = session.dispatch("node.model_preview", &json!({"canvas":entry.canvas,"nodes":[card],"model":model})).unwrap();
        session
            .dispatch(
                "node.set_model",
                &json!({"canvas":entry.canvas,"nodes":[card],"model":model,"expected_revision":preview["revision"],"preview_binding":preview["binding"]}),
            )
            .unwrap();
        let canvas = session.document().canvases().get(&entry.canvas).unwrap();
        assert_eq!(canvas.edges(), &edges);
        assert_eq!(canvas.nodes().keys().copied().collect::<Vec<_>>(), nodes);
        assert_eq!(canvas.nodes().get(&card).unwrap().params.get("model"), Some(&Value::Text(model.into())));
    }
}
#[test]
fn templates952_existing_duplicate_path_remaps_storyboard_sources_and_is_one_undo_step() {
    let mut project = Project::new(ProjectId::from_bytes([95; 16]), "Duplicate").unwrap();
    let base = CanvasId::from_bytes([1; 16]);
    Change::AddCanvas { canvas: Canvas::new(base, "Work") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let result = session.dispatch("template.add", &json!({"id":"storyboard"})).unwrap();
    let canvas: CanvasId = serde_json::from_value(result["canvas"].clone()).unwrap();
    let c = session.document().canvases().get(&canvas).unwrap();
    let nodes = c.nodes().values().cloned().collect::<Vec<_>>();
    let ids = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, nodecraft_types::NodeId::from_bytes([u8::try_from(i + 40).unwrap(); 16])))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut edges = c.edges().values().cloned().collect::<Vec<_>>();
    for (i, edge) in edges.iter_mut().enumerate() {
        edge.id = nodecraft_types::EdgeId::from_bytes([u8::try_from(i + 80).unwrap(); 16]);
    }
    let old_deck = nodes.iter().find(|n| n.kind == "structure.deck").unwrap().id;
    let before = session.document().clone();
    session.dispatch("canvas.paste", &json!({"canvas":canvas,"nodes":nodes,"edges":edges,"node_ids":ids})).unwrap();
    let node = session.document().canvases().get(&canvas).unwrap().nodes().get(ids.get(&old_deck).unwrap()).unwrap();
    let deck = nodecraft_actions::deck::Deck::parse(nodecraft_nodes::deck::stored(node).unwrap()).unwrap();
    for item in deck.slides.iter().flat_map(|s| &s.layers) {
        if let Some(layer) = item.layer()
            && let nodecraft_actions::layers::Content::Source { origin, .. } = &layer.content
        {
            assert!(ids.values().any(|id| id.to_string() == *origin));
        }
    }
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &before);
}
