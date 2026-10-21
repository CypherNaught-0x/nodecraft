//! What NodeCraft sends Comfy Cloud: the checked workflow, its inputs as assets, the idempotency key.
use super::*;
use crate::JobInput;
use crate::comfyui::{split_target, uuid_from};
use nodecraft_comfy::{ApiWorkflow, ComfyError, InputKind, InputValue};
use serde_json::{Map, Value};

/// The `Idempotency-Key` for a job: a UUID derived from its key, so every retry presents the same one.
pub(super) fn idempotency_key(job_key: &str) -> String {
    uuid_from(&format!("nodecraft-comfy-cloud\u{0}{job_key}"))
}

fn comfy_error(e: ComfyError) -> ProviderError {
    let kind = match e {
        ComfyError::TooLarge { .. } | ComfyError::Malformed { .. } => ErrorKind::Malformed,
        _ => ErrorKind::UnsupportedInput,
    };
    ProviderError::new(kind, e.to_string())
}

/// One media input to upload before the job is submitted.
pub(super) struct Upload {
    pub node: String,
    pub input: String,
    pub mime: &'static str,
    pub file_name: String,
    pub bytes: Arc<[u8]>,
}

/// A checked workflow and the inputs still to upload into it.
pub(super) struct Prepared {
    pub workflow: ApiWorkflow,
    pub uploads: Vec<Upload>,
}

/// Parses the job's workflow, applies its `values`, checks that every media input targets a file
/// input, and checks the result against the cloud's catalog. Sends nothing.
pub(super) fn prepare(job: &ProviderJob, catalog: &NodeCatalog, limits: Limits) -> Result<Prepared, ProviderError> {
    let raw = job.params.get("workflow").ok_or_else(|| invalid("the job has no ComfyUI workflow"))?;
    let bytes = serde_json::to_vec(raw).map_err(|e| invalid(e.to_string()))?;
    let mut workflow = ApiWorkflow::parse(&bytes, Some(catalog)).map_err(comfy_error)?;
    let empty = Map::new();
    let values = job.params.get("values").and_then(Value::as_object).unwrap_or(&empty);
    for (target, value) in values {
        let (node, input) = split_target(target)?;
        workflow.set_input(catalog, node, input, value.clone()).map_err(comfy_error)?;
    }
    // Partner nodes bill credits no quote here covers; refused before anything is uploaded or run.
    for (id, node) in &workflow.nodes {
        if catalog.get(&node.class_type).is_some_and(|info| info.api_node) {
            return Err(invalid(format!(
                "node {id} ({}) is a partner node, which Comfy Cloud bills separately; NodeCraft can't price it, so run that model through its own provider",
                node.class_type
            )));
        }
    }
    let mut uploads = Vec::new();
    for input in &job.inputs {
        let JobInput::Media { role, bytes, .. } = input else { continue };
        let (node, name) = split_target(role)?;
        let class = workflow.nodes.get(node).map(|n| n.class_type.clone()).ok_or_else(|| invalid(format!("no node {node} in the workflow")))?;
        let is_upload = catalog.get(&class).and_then(|i| i.input(name)).is_some_and(|spec| matches!(spec.kind, InputKind::Combo { upload: Some(_), .. }));
        if !is_upload {
            return Err(invalid(format!("{node}.{name} is not a file input")));
        }
        if bytes.len() > limits.input_bytes {
            return Err(invalid(format!("{node}.{name} is larger than Comfy Cloud's {} MiB upload limit", limits.input_bytes / MIB)));
        }
        let checked = crate::check_media(bytes, None).map_err(|e| ProviderError::new(ErrorKind::UnsupportedInput, e.message))?;
        if matches!(nodecraft_media::sniff(bytes).map(|s| s.kind), Some(nodecraft_media::MediaKind::Text | nodecraft_media::MediaKind::Document)) {
            return Err(invalid(format!("{node}.{name} takes a media file, not {}", checked.mime)));
        }
        if nodecraft_media::is_undecoded_image(checked.mime) {
            return Err(invalid(format!("can't upload {node}.{name}: NodeCraft can't read {} files yet; convert the input to PNG or JPEG", checked.mime)));
        }
        let ext = extension(checked.mime);
        uploads.push(Upload {
            node: node.to_owned(),
            input: name.to_owned(),
            mime: checked.mime,
            file_name: format!("nodecraft-{}.{ext}", checked.hash.to_hex()),
            bytes: Arc::clone(bytes),
        });
    }
    workflow.check(catalog).into_result().map_err(comfy_error)?;
    Ok(Prepared { workflow, uploads })
}

/// The usual extension for an upload of type `mime`; ComfyUI's loaders pick their decoder by it.
pub(super) fn extension(mime: &str) -> &str {
    match mime {
        "image/jpeg" => "jpg",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "audio/mpeg" => "mp3",
        "audio/x-wav" | "audio/wave" => "wav",
        "model/gltf-binary" => "glb",
        _ => mime.rsplit('/').next().unwrap_or("bin").split(['+', ';', '.']).next().filter(|e| !e.is_empty()).unwrap_or("bin"),
    }
}

impl Prepared {
    /// Points the upload's input at the stored asset. Comfy Cloud reads `core/ASSET` references
    /// in place of a file name, so this skips the catalog's combo check on purpose.
    pub(super) fn set_asset(&mut self, upload: &Upload, asset: &str) -> Result<(), ProviderError> {
        let node = self.workflow.nodes.get_mut(&upload.node).ok_or_else(|| invalid(format!("no node {} in the workflow", upload.node)))?;
        let reference = serde_json::json!({"__type": "core/ASSET", "info": {"id": asset, "file_path": upload.file_name}});
        node.inputs.insert(upload.input.clone(), InputValue::Value(reference));
        Ok(())
    }

    /// The `POST /api/v2/jobs` body. No `extra_data`: see the module docs on partner nodes.
    pub(super) fn body(&self) -> Result<Value, ProviderError> {
        Ok(serde_json::json!({"workflow": self.workflow.to_json().map_err(comfy_error)?}))
    }
}
