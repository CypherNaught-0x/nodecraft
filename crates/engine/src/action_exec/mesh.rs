//! Convert 3D (#111): decodes the connected model within the media limits and writes it as GLB
//! or OBJ, staged like any other action result.
use super::{LocalActions, word};
use nodecraft_media::{MeshFormat, MeshLimits, decode_mesh, encode_glb, encode_obj};
use nodecraft_types::{AssetRef, MediaMeta, ParamValues, Value};
use std::sync::PoisonError;

impl LocalActions {
    /// The bytes of the model `value` names: one of the project's, or one an earlier action in
    /// this run made.
    pub(super) fn model_bytes(&self, value: Option<&Value>) -> Result<Vec<u8>, String> {
        self.media_bytes(value, "3D model")
    }

    /// The bytes of the media `value` names (a `noun`, for messages): one of the project's, or
    /// one an earlier action in this run made.
    pub(super) fn media_bytes(&self, value: Option<&Value>, noun: &str) -> Result<Vec<u8>, String> {
        let Some(Value::Media(source)) = value else { return Err(format!("connect a {noun} first")) };
        if let Some(staged) = self.staged.lock().unwrap_or_else(PoisonError::into_inner).get(&source.hash) {
            return Ok(staged.to_vec());
        }
        if let Some(resident) = self.assets.get(&source.hash) {
            return Ok(resident.to_vec());
        }
        let input = self.assets.preview_read(source).map_err(|error| match error {
            nodecraft_format::PreviewReadError::Missing => format!("the input {noun} isn't in this project"),
            nodecraft_format::PreviewReadError::TooLarge => format!("the input {noun} is too large to read"),
            _ => format!("the input {noun} can't be read"),
        })?;
        input.read_verified(&Default::default(), &Default::default()).map_err(|_| format!("the input {noun} can't be read"))
    }

    pub(super) fn convert_mesh(&self, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        let format = match word(params, "format")? {
            "glb" => MeshFormat::Glb,
            "obj" => MeshFormat::Obj,
            other => return Err(format!("{other} isn't a 3D format NodeCraft writes")),
        };
        let bytes = self.model_bytes(inputs.first())?;
        let mesh = decode_mesh(&bytes, &MeshLimits::default()).map_err(|e| format!("the input model can't be read: {e}"))?;
        let encoded = match format {
            MeshFormat::Obj => encode_obj(&mesh),
            _ => encode_glb(&mesh),
        }
        .map_err(|e| format!("the model can't be written: {e}"))?;
        if encoded.len() > nodecraft_format::MAX_ASSET_BYTES {
            return Err(format!("the converted model is larger than {} MiB", nodecraft_format::MAX_ASSET_BYTES / (1024 * 1024)));
        }
        let hash = self.staged.lock().unwrap_or_else(PoisonError::into_inner).insert(&encoded).map_err(|e| e.message)?;
        let mime = format.mime().parse().map_err(|_| "the 3D format has no media type".to_owned())?;
        let info = mesh.info();
        let meta = MediaMeta { vertices: u32::try_from(info.vertices).ok(), triangles: u32::try_from(info.triangles).ok(), ..MediaMeta::default() };
        Ok(Value::Media(AssetRef { hash, mime, meta }))
    }
}
