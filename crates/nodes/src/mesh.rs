//! 3D models (#111): the 3D source node, and Convert 3D, which writes a model in another format
//! as a new result (the source model is never changed). Captures (#112) draw the model offscreen
//! into an ordinary image: a photo from one camera, or a sheet of views round it.
use crate::SourceKind;
use crate::action::{ActionKind, choice, color, count, kind, port, slider, when};
use nodecraft_types::{Error, Modality, ParamSpec};

/// The 3D source kind.
pub const SOURCE: &str = "source.mesh";
/// The Convert 3D action kind.
pub const CONVERT: &str = "mesh.convert";
/// The 3D photo action kind: one view of the model.
pub const PHOTO: &str = "mesh.photo";
/// The 3D view sheet action kind: 4, 8 or 16 views round the model, in one image.
pub const VIEWS: &str = "mesh.views";
/// A soundless video of a saved orbit around a model.
pub const TURNTABLE: &str = "mesh.turntable";
pub mod turntable;
/// The longest side of a photo, and of one view on a sheet.
pub const MAX_PHOTO_SIDE: u64 = 2048;
pub const MAX_VIEW_SIDE: u64 = 1024;

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

/// The camera and look a capture shares with the viewer: `pitch` (degrees above the model),
/// `zoom` (1 fits the model), the style, and the background (`#rrggbbaa`, transparent by
/// default) with the wireframe's line colour.
fn look() -> Result<Vec<ParamSpec>, Error> {
    Ok(vec![
        slider("pitch", "Elevation", -88.0, 88.0, 1.0, 20.0)?,
        slider("zoom", "Zoom", 0.25, 16.0, 0.05, 1.0)?,
        choice("style", "Style", &[("shaded", "Shaded"), ("unlit", "Unlit"), ("wireframe", "Wireframe")], "shaded"),
        color("background", "Background", "#00000000"),
        when(color("line", "Line colour", "#e6e6e6ff"), "style", "wireframe"),
    ])
}

/// One view of the connected model, as an image. `yaw` is the camera's angle round the model in
/// degrees (0 looks at its front, along -z); the camera keeps +y up.
pub fn photo_kind() -> Result<ActionKind, Error> {
    let mut params = vec![slider("yaw", "Angle", -180.0, 180.0, 1.0, 34.0)?];
    params.extend(look()?);
    params.push(count("width", "Width", 16, MAX_PHOTO_SIDE, 1024));
    params.push(count("height", "Height", 16, MAX_PHOTO_SIDE, 1024));
    Ok(kind(PHOTO, "3D photo", vec![port("model", Modality::Mesh, false)], vec![port("out", Modality::Image, false)], params))
}

/// 4, 8 or 16 views evenly round the connected model, as one contact sheet: 2 × 2, 4 × 2 or
/// 4 × 4, read left to right and top to bottom. View `i` looks from `start + i × 360° / count`,
/// the camera moving counter-clockwise seen from above (or clockwise).
pub fn views_kind() -> Result<ActionKind, Error> {
    let mut params = vec![
        choice("count", "Views", &[("4", "4"), ("8", "8"), ("16", "16")], "8"),
        slider("start", "First angle", -180.0, 180.0, 1.0, 0.0)?,
        choice("direction", "Direction", &[("ccw", "Counter-clockwise"), ("cw", "Clockwise")], "ccw"),
    ];
    params.extend(look()?);
    params.push(count("size", "View size", 64, MAX_VIEW_SIDE, 512));
    Ok(kind(VIEWS, "3D view sheet", vec![port("model", Modality::Mesh, false)], vec![port("out", Modality::Image, false)], params))
}

/// Columns and rows of a sheet of `count` views.
pub fn sheet_grid(count: u32) -> (u32, u32) {
    match count {
        0..=4 => (2, 2),
        5..=8 => (4, 2),
        _ => (4, 4),
    }
}
