//! 3D models (#111): the 3D source node, and Convert 3D, which writes a model in another format
//! as a new result (the source model is never changed).
use crate::SourceKind;
use crate::action::{ActionKind, choice, kind, port};
use nodecraft_types::Modality;

/// The 3D source kind.
pub const SOURCE: &str = "source.mesh";
/// The Convert 3D action kind.
pub const CONVERT: &str = "mesh.convert";

/// A 3D model (glTF, GLB or OBJ), shown in an orbit viewer on its card.
pub fn kind_source() -> SourceKind {
    SourceKind::new(SOURCE, "3D", "3", Modality::Mesh)
}

/// Writes the connected model as GLB or OBJ. Both keep positions, normals and vertex colours;
/// textures were already baked into vertex colours when the model was read.
pub fn convert_kind() -> ActionKind {
    kind(
        CONVERT,
        "Convert 3D",
        vec![port("model", Modality::Mesh, false)],
        vec![port("out", Modality::Mesh, false)],
        vec![choice("format", "Format", &[("glb", "GLB (binary glTF)"), ("obj", "OBJ")], "glb")],
    )
}
