//! Original single-view core-node graphs, authored from public node interfaces (#24).
//! The fixtures project documentation, not a live server's `/object_info`.
use super::*;
use crate::provider::Output;

const HANDLE_KEY: &str = "comfy_mesh_model_v1";

pub(crate) fn supports(model: &str) -> bool {
    matches!(model, "hunyuan3d-2.0" | "hunyuan3d-2.1")
}

pub(super) fn expand(job: &ProviderJob, catalog: &NodeCatalog) -> Result<ProviderJob, (ProviderError, bool)> {
    let plain = |e: ProviderError| (e, false);
    let checkpoint_default = if job.model == "hunyuan3d-2.1" { "hunyuan_3d_v2.1.safetensors" } else { "hunyuan3d-dit-v2.safetensors" };
    let checkpoint = match job.params.get("checkpoint") {
        None | Some(Value::Null) => checkpoint_default,
        Some(Value::String(value)) if !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control) => value,
        _ => return Err(plain(refused("`checkpoint` must name a checkpoint on the server (at most 512 bytes)"))),
    };
    if let Some(installed) = catalog.options("ImageOnlyCheckpointLoader", "ckpt_name")
        && !installed.iter().any(|name| name == checkpoint)
    {
        return Err((
            refused(format!(
                "the server has no checkpoint {checkpoint:?}; install it in ComfyUI's models/checkpoints folder, or choose an installed checkpoint"
            )),
            true,
        ));
    }
    let image = match job.inputs.as_slice() {
        [JobInput::Media { role, mime, bytes }] if role == "image" => (mime.clone(), bytes.clone()),
        _ => return Err(plain(refused("this Hunyuan3D workflow needs exactly one image and has no text prompt input"))),
    };
    let steps = whole(job, "steps").map_err(plain)?.unwrap_or(30);
    if !(1..=100).contains(&steps) {
        return Err(plain(refused("`steps` must be between 1 and 100")));
    }
    let cfg = match job.params.get("cfg") {
        None | Some(Value::Null) => 5.0,
        Some(value) => {
            value.as_f64().filter(|value| value.is_finite() && (1.0..=20.0).contains(value)).ok_or_else(|| plain(refused("`cfg` must be between 1 and 20")))?
        }
    };
    let seed = seed(job).map_err(plain)?;
    let mut nodes = Map::new();
    let mut node = |id: &str, class: &str, inputs: Value| nodes.insert(id.to_owned(), json!({"class_type": class, "inputs": inputs}));
    node("1", "ImageOnlyCheckpointLoader", json!({"ckpt_name":checkpoint}));
    node("2", "LoadImage", json!({"image":IMAGE_PLACEHOLDER}));
    node("3", "CLIPVisionEncode", json!({"clip_vision":["1",1],"image":["2",0],"crop":"center"}));
    node("4", "Hunyuan3Dv2Conditioning", json!({"clip_vision_output":["3",0]}));
    node("5", "EmptyLatentHunyuan3Dv2", json!({"resolution":3072,"batch_size":1}));
    let model = if job.model == "hunyuan3d-2.1" {
        node("6", "ModelSamplingAuraFlow", json!({"model":["1",0],"shift":1.0}));
        "6"
    } else {
        "1"
    };
    node(
        "7",
        "KSampler",
        json!({"model":[model,0],"positive":["4",0],"negative":["4",1],"latent_image":["5",0],"seed":seed,"steps":steps,"cfg":cfg,"sampler_name":"euler","scheduler":"normal","denoise":1.0}),
    );
    node("8", "VAEDecodeHunyuan3D", json!({"samples":["7",0],"vae":["1",2],"num_chunks":8000,"octree_resolution":256}));
    node("9", "VoxelToMesh", json!({"voxel":["8",0],"algorithm":"surface net","threshold":0.6}));
    node("10", "SaveGLB", json!({"mesh":["9",0],"filename_prefix":"mesh/NodeCraft"}));
    fill_defaults(&mut nodes, catalog);
    let mut params = Map::new();
    params.insert("workflow".into(), Value::Object(nodes));
    Ok(ProviderJob {
        model: job.model.clone(),
        params,
        inputs: vec![JobInput::Media { role: "2.image".into(), mime: image.0, bytes: image.1 }],
        idempotency_key: job.idempotency_key.clone(),
    })
}

/// Store the named preset's output contract with the durable handle, including deduplicated
/// submissions. A general imported workflow may legitimately return several media kinds.
pub(crate) fn save_policy(mut data: Value, job: &ProviderJob) -> Result<Value, ProviderError> {
    if !is_templated(job) || !supports(&job.model) {
        return Ok(data);
    }
    if data.is_null() {
        data = json!({});
    }
    let object = data.as_object_mut().ok_or_else(|| refused("the ComfyUI result handle is malformed"))?;
    object.insert(HANDLE_KEY.into(), Value::String(job.model.clone()));
    Ok(data)
}

/// Posters/textures from a mesh preset have no Image output port. Keep the exact mesh files;
/// generic and legacy handles retain their existing mixed-output behavior.
pub(crate) fn policy(data: &Value) -> Result<bool, ProviderError> {
    match data.get(HANDLE_KEY) {
        None => Ok(false),
        Some(Value::String(model)) if supports(model) => Ok(true),
        _ => Err(refused("the saved ComfyUI mesh preset is malformed or unsupported")),
    }
}

pub(crate) fn outputs(data: &Value, mut outputs: Vec<Output>) -> Result<Vec<Output>, ProviderError> {
    if !policy(data)? {
        return Ok(outputs);
    }
    outputs.retain(|output| matches!(output, Output::Bytes {mime,..} if nodecraft_media::mesh::MeshFormat::from_mime(mime).is_some()));
    if outputs.is_empty() {
        return Err(ProviderError::new(ErrorKind::JobFailed, "the 3D workflow saved no supported mesh; previews and texture images are not meshes"));
    }
    for output in &outputs {
        if let Output::Bytes { bytes, .. } = output {
            nodecraft_media::mesh::probe_mesh(bytes, &Default::default())
                .map_err(|_| ProviderError::new(ErrorKind::Malformed, "a saved 3D result is damaged or exceeds NodeCraft's mesh limits"))?;
        }
    }
    Ok(outputs)
}
