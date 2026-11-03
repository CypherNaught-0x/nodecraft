use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Project};
use nodecraft_types::ProjectId;
use std::sync::Arc;

const C: CanvasId = CanvasId::from_bytes([0x97; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const NEW: NodeId = NodeId::from_bytes([2; 16]);
const WORKFLOW: NodeId = NodeId::from_bytes([3; 16]);
const COPY: NodeId = NodeId::from_bytes([4; 16]);
const OBJECT_INFO: &str = include_str!("../../tests/fixtures/comfy_object_info970.json");
/// A plain SDXL graph, as an imported workflow.
const SDXL_WORKFLOW: &str = r#"{
  "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sd_xl_base_1.0.safetensors"}},
  "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a fox", "clip": ["1", 1]}},
  "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["1", 1]}},
  "4": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 1}},
  "5": {"class_type": "KSampler", "inputs": {"model": ["1", 0], "seed": 7, "steps": 20, "cfg": 7.0, "sampler_name": "euler", "scheduler": "normal",
        "positive": ["2", 0], "negative": ["3", 0], "latent_image": ["4", 0], "denoise": 1.0}},
  "6": {"class_type": "VAEDecode", "inputs": {"samples": ["5", 0], "vae": ["1", 2]}},
  "7": {"class_type": "SaveImage", "inputs": {"images": ["6", 0], "filename_prefix": "nc"}}
}"#;

fn nodes() -> Arc<NodeCatalog> {
    Arc::new(NodeCatalog::parse(OBJECT_INFO.as_bytes()).unwrap().0)
}

/// A session with an image card on another provider's model.
fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0x97; 16]), "Server models").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let values: ParamValues = [("model".into(), Value::Text("openai-images.sunburst".into())), ("prompt".into(), Value::Text("a lighthouse".into()))].into();
    let node = s.kinds().get(generation::KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    s.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
    s
}

/// [`session`] after the default server `studio` was read.
fn ready() -> Session {
    let mut s = session();
    crate::comfy_server_cmds::finish(&mut s, 1, "studio", true, Ok(nodes().into()));
    s
}

fn group<'a>(list: &'a Json, id: &str) -> &'a Json {
    list["groups"].as_array().unwrap().iter().find(|g| g["id"] == id).unwrap_or_else(|| panic!("no group {id}: {list}"))
}

fn names(group: &Json) -> Vec<&str> {
    group["models"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect()
}

fn card(s: &Session, id: NodeId) -> &Node {
    s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap()
}

#[test]
fn before_a_read_nothing_is_listed_and_every_choice_says_to_refresh() {
    let mut s = session();
    let list = s.dispatch("comfy.models.list", &json!({})).unwrap();
    assert_eq!((list["read"].clone(), list["server"].clone(), list["groups"].clone()), (json!(false), Json::Null, json!([])));
    let error = s.dispatch("comfy.models.preview", &json!({"canvas": C, "nodes": [CARD], "group": "checkpoints", "name": "x"})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("comfy.nodes.refresh"), "{}", error.message);
}

#[test]
fn the_server_list_is_grouped_by_type_and_says_what_runs_and_why_not() {
    let mut s = ready();
    let list = s.dispatch("comfy.models.list", &json!({})).unwrap();
    assert_eq!((list["read"].clone(), list["server"].clone()), (json!(true), json!("studio")), "the list says which server it came from");
    let ids: Vec<&str> = list["groups"].as_array().unwrap().iter().map(|g| g["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["checkpoints", "loras", "vaes", "text_encoders", "diffusion_models", "upscalers"], "a type the server lacks is left out");

    let checkpoints = group(&list, "checkpoints");
    let unique: BTreeSet<&str> = list["groups"].as_array().unwrap().iter().flat_map(names).collect();
    let all: Vec<&str> = list["groups"].as_array().unwrap().iter().flat_map(names).collect();
    assert_eq!(unique.len(), all.len(), "the image-only loader's copy of the checkpoints folder isn't listed twice: {all:?}");
    assert_eq!(
        names(checkpoints),
        ["sd_xl_base_1.0.safetensors", "juggernaut_xl_v9.safetensors", "flux1-dev-fp8.safetensors", "svd_xt.safetensors", "sd3.5_large.safetensors"]
    );
    let models = checkpoints["models"].as_array().unwrap();
    assert_eq!(models[1]["model"], "comfyui.sdxl");
    assert_eq!(models[2]["model"], "comfyui.flux-dev");
    assert!(models[3]["unavailable"].as_str().unwrap().contains("video"), "{}", models[3]);
    assert!(models[4]["unavailable"].as_str().unwrap().contains("SD3"), "{}", models[4]);
    assert!(models[3].get("model").is_none());

    let loras = group(&list, "loras");
    assert_eq!(names(loras), ["detail_tweaker.safetensors", "watercolor_style.safetensors"]);
    assert!(loras["unavailable"].as_str().unwrap().contains("LoRA"), "a group-wide reason: {loras}");
    for id in ["vaes", "text_encoders", "diffusion_models", "upscalers"] {
        assert!(group(&list, id)["unavailable"].is_string(), "{id} says why it can't run on its own");
    }
}

#[test]
fn checkpoint_names_route_conservatively() {
    let sdxl = |note| Ok(Route { model: "comfyui.sdxl", note });
    for (name, route) in [
        ("juggernautXL_v9.safetensors", sdxl(None)),
        ("ponyDiffusionV6.safetensors", sdxl(None)),
        ("illustriousXL_v01.safetensors", sdxl(None)),
        ("FLUX1-Schnell.safetensors", Ok(Route { model: "comfyui.flux-dev", note: None })),
        ("InfluxRealism.safetensors", sdxl(Some(UNKNOWN_NOTE))),
        ("realisticVision_v51.safetensors", sdxl(Some(UNKNOWN_NOTE))),
        ("wanderer.safetensors", sdxl(Some(UNKNOWN_NOTE))),
        ("v1-5-pruned-emaonly.safetensors", sdxl(Some(SD12_NOTE))),
        ("sd21-768-v.safetensors", sdxl(Some(SD12_NOTE))),
        ("flux1-kontext-dev.safetensors", Err(FLUX_TOOL)),
        ("flux1-fill-dev.safetensors", Err(FLUX_TOOL)),
        ("sd_xl_refiner_1.0.safetensors", Err(OTHER_ARCHITECTURE)),
        ("juggernaut_xl_inpainting.safetensors", Err(OTHER_ARCHITECTURE)),
        ("qwen_image_fp8.safetensors", Err(OTHER_ARCHITECTURE)),
        ("wan2.1_t2v_1.3B_fp16.safetensors", Err(VIDEO_CHECKPOINT)),
        ("ltxv-13b-0.9.7-dev.safetensors", Err(VIDEO_CHECKPOINT)),
        ("hunyuan_video_720.safetensors", Err(VIDEO_CHECKPOINT)),
        ("stable_audio_open_1.0.safetensors", Err(AUDIO_CHECKPOINT)),
        ("ace_step_v1_3.5b.safetensors", Err(AUDIO_CHECKPOINT)),
        ("sd3_medium_incl_clips.safetensors", Err(SD3_CHECKPOINT)),
        ("sd_3.5_large.safetensors", Err(SD3_CHECKPOINT)),
        ("StableDiffusion3-medium.safetensors", Err(SD3_CHECKPOINT)),
    ] {
        assert_eq!(checkpoint_route(&name.to_lowercase()), route, "{name}");
    }
}

#[test]
fn hostile_names_are_bounded_and_never_fill_the_list() {
    let long = "a".repeat(MAX_NAME_BYTES + 10);
    let many: Vec<String> = (0..MAX_ENTRIES + 5).map(|n| format!("m{n}.safetensors")).collect();
    let mut listed = vec![long.clone(), "bad\u{7}name".into(), "spoof\u{202e}name".into(), "dup.safetensors".into(), "dup.safetensors".into()];
    listed.extend(many);
    let info = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [listed, {}]}}, "output": ["MODEL"]}});
    let catalog = NodeCatalog::parse(&serde_json::to_vec(&info).unwrap()).unwrap().0;
    let groups = inventory(&catalog, None);
    let checkpoints = groups.first().unwrap();
    assert_eq!(checkpoints.models.len(), MAX_GROUP_ENTRIES, "one group can't take the whole listing");
    assert_eq!(checkpoints.skipped, 3, "the control character, the bidi override and the repeat");
    assert_eq!(checkpoints.more, MAX_ENTRIES + 5 + 2 - MAX_GROUP_ENTRIES);
    let first = checkpoints.models.first().unwrap();
    assert!(first.name.len() <= MAX_NAME_BYTES + 3 && first.unavailable.as_deref() == Some(TOO_LONG), "{first:?}");
}

#[test]
fn a_checkpoint_from_the_server_is_put_on_the_card_in_one_undo_step() {
    let mut s = ready();
    let request = json!({"canvas": C, "nodes": [CARD], "group": "checkpoints", "name": "juggernaut_xl_v9.safetensors"});
    let before = s.document().clone();
    let preview = s.dispatch("comfy.models.preview", &request).unwrap();
    assert_eq!(s.document(), &before, "a preview changes nothing");
    assert_eq!(preview["model"], "comfyui.sdxl");
    assert_eq!(preview["server_model"]["name"], "juggernaut_xl_v9.safetensors");
    assert!(preview["targets"][0]["kept"].as_array().unwrap().contains(&json!("checkpoint")));
    let mut commit = request.clone();
    commit["expected_revision"] = preview["revision"].clone();
    commit["preview_binding"] = preview["binding"].clone();

    // A preview for another checkpoint doesn't apply this one.
    let mut other = commit.clone();
    other["name"] = json!("sd_xl_base_1.0.safetensors");
    assert_eq!(s.dispatch("comfy.models.use", &other).unwrap_err().kind, ErrorKind::Conflict);

    s.dispatch("comfy.models.use", &commit).unwrap();
    let node = card(&s, CARD);
    assert_eq!(node.params.get("model"), Some(&Value::Text("comfyui.sdxl".into())));
    assert_eq!(node.params.get("checkpoint"), Some(&Value::Text("juggernaut_xl_v9.safetensors".into())));
    assert_eq!(node.params.get("prompt"), Some(&Value::Text("a lighthouse".into())));
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(s.document(), &before, "model and checkpoint come back together");
}

#[test]
fn a_model_the_server_lacks_or_cant_run_is_refused_not_replaced() {
    let mut s = ready();
    let before = s.document().clone();
    for (group, name, kind) in [
        ("checkpoints", "not_installed.safetensors", ErrorKind::NotFound),
        ("loras", "not_installed.safetensors", ErrorKind::NotFound),
        ("checkpoints", "svd_xt.safetensors", ErrorKind::Unsupported),
        ("loras", "detail_tweaker.safetensors", ErrorKind::Unsupported),
        ("video_checkpoints", "svd_xt.safetensors", ErrorKind::InvalidInput),
        ("nonsense", "x", ErrorKind::InvalidInput),
    ] {
        let request = json!({"canvas": C, "nodes": [CARD], "group": group, "name": name});
        assert_eq!(s.dispatch("comfy.models.preview", &request).unwrap_err().kind, kind, "{group} {name}");
        let add = json!({"canvas": C, "id": NEW, "pos": {"x": 0, "y": 0}, "group": group, "name": name});
        assert_eq!(s.dispatch("comfy.models.add", &add).unwrap_err().kind, kind, "{group} {name}");
    }
    let message = s
        .dispatch("comfy.models.preview", &json!({"canvas": C, "nodes": [CARD], "group": "checkpoints", "name": "secret-name.safetensors"}))
        .unwrap_err()
        .message;
    assert!(message.contains("substituted") && !message.contains("secret-name"), "{message}");
    assert_eq!(s.document(), &before);
}

#[test]
fn a_new_card_is_placed_for_a_server_checkpoint() {
    let mut s = ready();
    let add = json!({"canvas": C, "id": NEW, "pos": {"x": 40, "y": 60}, "group": "checkpoints", "name": "flux1-dev-fp8.safetensors"});
    let result = s.dispatch("comfy.models.add", &add).unwrap();
    assert_eq!(result["model"], "comfyui.flux-dev");
    let node = card(&s, NEW);
    assert_eq!(node.kind, generation::KIND);
    assert_eq!(node.params.get("model"), Some(&Value::Text("comfyui.flux-dev".into())));
    assert_eq!(node.params.get("checkpoint"), Some(&Value::Text("flux1-dev-fp8.safetensors".into())));
    assert_eq!((node.pos.x, node.pos.y), (40.0, 60.0));
}

#[test]
fn the_current_checkpoint_is_shown_present_or_missing() {
    let mut s = ready();
    let before = s.dispatch("comfy.models.list", &json!({"canvas": C, "nodes": [CARD]})).unwrap();
    assert_eq!(before["current"], json!([]), "a card on another provider has no server checkpoint");
    let add = json!({"canvas": C, "id": NEW, "pos": {"x": 0, "y": 0}, "group": "checkpoints", "name": "juggernaut_xl_v9.safetensors"});
    s.dispatch("comfy.models.add", &add).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": C, "id": NEW, "name": "checkpoint", "value": {"type": "text", "value": "gone.safetensors"}})).unwrap();
    let list = s.dispatch("comfy.models.list", &json!({"canvas": C, "nodes": [CARD, NEW]})).unwrap();
    assert_eq!(list["current"], json!([{"id": NEW, "checkpoint": "gone.safetensors", "present": false}]));
    assert_eq!(card(&s, NEW).params.get("checkpoint"), Some(&Value::Text("gone.safetensors".into())), "missing is shown, never swapped");
}

#[test]
fn imported_workflows_are_listed_by_title_and_copied_with_their_mapping() {
    let mut s = ready();
    let import = json!({"canvas": C, "id": WORKFLOW, "pos": {"x": 0, "y": 0}, "workflow": SDXL_WORKFLOW, "title": "Fox portrait"});
    s.dispatch("comfy.workflow.import", &import).unwrap();
    s.dispatch("comfy.server.use", &json!({"canvas": C, "id": WORKFLOW, "server": "studio"})).unwrap();
    let list = s.dispatch("comfy.models.list", &json!({})).unwrap();
    assert_eq!(list["workflows"], json!([{"canvas": C, "id": WORKFLOW, "title": "Fox portrait", "outputs": ["image"], "modalities": ["image"]}]));

    let add = json!({"canvas": C, "id": COPY, "pos": {"x": 300, "y": 0}, "workflow": {"canvas": C, "id": WORKFLOW}});
    s.dispatch("comfy.models.add", &add).unwrap();
    let (source, copy) = (card(&s, WORKFLOW).clone(), card(&s, COPY).clone());
    assert_eq!(copy.kind, comfy_workflow::ID);
    assert_eq!(copy.title, "Fox portrait");
    assert_eq!(copy.config.get(comfy_workflow::MAPPING), source.config.get(comfy_workflow::MAPPING), "the saved mapping comes along");
    assert_eq!(copy.config.get(comfy_workflow::WORKFLOW), source.config.get(comfy_workflow::WORKFLOW));
    assert_eq!(copy.params, source.params);
    assert!(!copy.config.contains_key(crate::provider_setup::COMFY_SERVER_CONFIG), "the copy follows the default server");
    assert!(copy.history.is_empty());

    let not_a_workflow = json!({"canvas": C, "id": NEW, "pos": {"x": 0, "y": 0}, "workflow": {"canvas": C, "id": CARD}});
    assert_eq!(s.dispatch("comfy.models.add", &not_a_workflow).unwrap_err().kind, ErrorKind::InvalidInput);
    let both = json!({"canvas": C, "id": NEW, "pos": {"x": 0, "y": 0}, "group": "checkpoints", "name": "x", "workflow": {"canvas": C, "id": WORKFLOW}});
    assert_eq!(s.dispatch("comfy.models.add", &both).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn the_list_follows_the_default_server() {
    let mut s = ready();
    crate::comfy_server_cmds::finish(&mut s, 2, "laptop", false, Ok(nodes().into()));
    assert_eq!(s.dispatch("comfy.models.list", &json!({})).unwrap()["server"], "studio", "another server's check doesn't replace the default's list");
    crate::comfy_server_cmds::forget(&mut s, "studio", true);
    let list = s.dispatch("comfy.models.list", &json!({})).unwrap();
    assert_eq!((list["read"].clone(), list["server"].clone()), (json!(false), Json::Null), "a new default lists nothing until it is read");
}

fn commit_request(s: &mut Session, name: &str) -> Json {
    let mut request = json!({"canvas": C, "nodes": [CARD], "group": "checkpoints", "name": name});
    let preview = s.dispatch("comfy.models.preview", &request).unwrap();
    request["expected_revision"] = preview["revision"].clone();
    request["preview_binding"] = preview["binding"].clone();
    request
}

#[test]
fn a_use_is_refused_after_a_refresh_removes_the_name() {
    let mut s = ready();
    let commit = commit_request(&mut s, "juggernaut_xl_v9.safetensors");
    let without = OBJECT_INFO.replace("\"juggernaut_xl_v9.safetensors\", ", "");
    crate::comfy_server_cmds::finish(&mut s, 2, "studio", true, Ok(Arc::new(NodeCatalog::parse(without.as_bytes()).unwrap().0).into()));
    let before = s.document().clone();
    assert_eq!(s.dispatch("comfy.models.use", &commit).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(s.document(), &before);
}

#[test]
fn the_binding_changes_with_the_default_server_and_its_reads() {
    let mut s = ready();
    let first = commit_request(&mut s, "juggernaut_xl_v9.safetensors");
    crate::comfy_server_cmds::finish(&mut s, 2, "laptop", true, Ok(nodes().into()));
    let second = commit_request(&mut s, "juggernaut_xl_v9.safetensors");
    assert_ne!(first["preview_binding"], second["preview_binding"], "another default server lists it now");
    assert_eq!(s.dispatch("comfy.models.use", &first).unwrap_err().kind, ErrorKind::Conflict);
    crate::comfy_server_cmds::finish(&mut s, 3, "laptop", true, Ok(nodes().into()));
    assert_eq!(s.dispatch("comfy.models.use", &second).unwrap_err().kind, ErrorKind::Conflict, "a later read of the same server");
    let third = commit_request(&mut s, "juggernaut_xl_v9.safetensors");
    s.dispatch("comfy.models.use", &third).unwrap();
}

#[test]
fn text_and_video_cards_are_refused() {
    let mut s = ready();
    for (id, kind) in [(NodeId::from_bytes([8; 16]), generation::TEXT_KIND), (NodeId::from_bytes([9; 16]), generation::VIDEO_KIND)] {
        let node = s.kinds().get(kind).unwrap().instantiate_instance(id, Default::default(), &ParamValues::new()).unwrap();
        s.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
        let request = json!({"canvas": C, "nodes": [id], "group": "checkpoints", "name": "juggernaut_xl_v9.safetensors"});
        assert_eq!(s.dispatch("comfy.models.preview", &request).unwrap_err().kind, ErrorKind::Unsupported, "{kind}");
    }
}
