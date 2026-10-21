//! 3D captures (#112): 3D photo and 3D view sheet draw the connected model offscreen, the way its
//! card's viewer draws it, and stage the result as an ordinary PNG. The camera, style and
//! background are the node's parameters (saved with the project), so a capture repeats exactly;
//! the viewer's own orbit is view state and only seeds them (`mesh.capture`).
use super::{LocalActions, capture_state, color, number, whole, word};
use nodecraft_actions::image::{ImageLimits, Raster};
use nodecraft_gpu::mesh::capture::{CaptureError, MeshCapture};
use nodecraft_gpu::mesh::{MeshData, Orbit, Style, View};
use nodecraft_media::{MeshLimits, decode_mesh};
use nodecraft_nodes::mesh::{MAX_PHOTO_SIDE, MAX_VIEW_SIDE, PHOTO, VIEWS, sheet_grid};
use nodecraft_types::{ParamValues, Value};

/// Most edges the wireframe style draws (as the viewer).
const MAX_EDGES: usize = 6_000_000;

/// `#rrggbb` or `#rrggbbaa` as linear RGBA, as the renderer takes it.
fn linear(text: &str) -> Result<[f32; 4], String> {
    let json = color(text)?;
    let channel = |name: &str| json.get(name).and_then(serde_json::Value::as_u64).map_or(0.0, |v| v.min(255) as f32 / 255.0);
    let decode = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    Ok([decode(channel("r")), decode(channel("g")), decode(channel("b")), channel("a")])
}

fn style(params: &ParamValues) -> Result<Style, String> {
    match word(params, "style")? {
        "shaded" => Ok(Style::Shaded),
        "unlit" => Ok(Style::Unlit),
        "wireframe" => Ok(Style::Wireframe),
        other => Err(format!("{other} isn't a 3D style")),
    }
}

fn degrees(params: &ParamValues, name: &str) -> Result<f32, String> {
    let value = number(params, name)?;
    if !value.is_finite() {
        return Err(format!("the {name} setting isn't a number"));
    }
    Ok(value.clamp(-360.0, 360.0).to_radians() as f32)
}

fn side(params: &ParamValues, name: &str, max: u64) -> Result<u32, String> {
    u32::try_from(whole(params, name)?.clamp(1, max)).map_err(|_| format!("the {name} setting is out of range"))
}

fn message(error: CaptureError) -> String {
    match error {
        CaptureError::Unavailable => "3D captures need a GPU, and none is available".to_owned(),
        CaptureError::Model(why) => format!("the model can't be drawn: {why}"),
        other => format!("the capture failed: {other}"),
    }
}

/// What to draw: the camera angles, the size of each view, and the sheet's columns and rows.
struct Shot {
    yaws: Vec<f32>,
    size: [u32; 2],
    grid: (u32, u32),
}

fn shot(action: &str, params: &ParamValues) -> Result<Shot, String> {
    if action == PHOTO {
        let size = [side(params, "width", MAX_PHOTO_SIDE)?, side(params, "height", MAX_PHOTO_SIDE)?];
        return Ok(Shot { yaws: vec![degrees(params, "yaw")?], size, grid: (1, 1) });
    }
    if action != VIEWS {
        return Err(format!("{action} isn't a 3D capture"));
    }
    let count: u32 = match word(params, "count")? {
        "4" => 4,
        "8" => 8,
        "16" => 16,
        other => return Err(format!("a view sheet has 4, 8 or 16 views, not {other}")),
    };
    let turn = match word(params, "direction")? {
        "ccw" => 1.0,
        "cw" => -1.0,
        other => return Err(format!("{other} isn't a direction")),
    };
    let start = degrees(params, "start")?;
    let step = std::f32::consts::TAU / count as f32 * turn;
    let size = side(params, "size", MAX_VIEW_SIDE)?;
    Ok(Shot { yaws: (0..count).map(|i| start + step * i as f32).collect(), size: [size, size], grid: sheet_grid(count) })
}

impl LocalActions {
    pub(super) fn capture_mesh(&self, action: &str, params: &ParamValues, inputs: &[Value]) -> Result<Value, String> {
        let shot = shot(action, params)?;
        let style = style(params)?;
        let pitch = degrees(params, "pitch")?;
        let zoom = number(params, "zoom")? as f32;
        let background = linear(word(params, "background")?)?;
        // Hidden unless the style is wireframe.
        let line = linear(word(params, "line").unwrap_or("#e6e6e6ff"))?;
        let bytes = self.model_bytes(inputs.first())?;
        let mesh = decode_mesh(&bytes, &MeshLimits::default()).map_err(|e| format!("the input model can't be read: {e}"))?;
        let bounds = mesh.bounds().ok_or("the input model has nothing to draw")?;
        let edges = if style == Style::Wireframe { mesh.edges(MAX_EDGES) } else { None };
        let data = MeshData { positions: &mesh.positions, normals: &mesh.normals, colors: &mesh.colors, indices: &mesh.indices, edges: edges.as_deref() };
        let (width, height, pixels) = capture_state::with(|slot| {
            if slot.as_ref().is_none_or(MeshCapture::is_lost) {
                *slot = Some(MeshCapture::request().map_err(message)?);
            }
            let capture = slot.as_ref().ok_or_else(|| message(CaptureError::Unavailable))?;
            let gpu = capture.upload(data).map_err(message)?;
            let target = capture.target(shot.size).map_err(message)?;
            let [w, h] = target.size();
            let (columns, rows) = shot.grid;
            let (sheet_w, sheet_h) = (w.saturating_mul(columns), h.saturating_mul(rows));
            let row = usize::try_from(w).map_err(|_| "the capture is too large")?.saturating_mul(4);
            let sheet_row = usize::try_from(sheet_w).map_err(|_| "the capture is too large")?.saturating_mul(4);
            let total = sheet_row.saturating_mul(usize::try_from(sheet_h).map_err(|_| "the capture is too large")?);
            let mut sheet = Vec::new();
            sheet.try_reserve_exact(total).map_err(|_| "there isn't enough memory for the capture")?;
            sheet.resize(total, 0);
            for (index, yaw) in (0u32..).zip(&shot.yaws) {
                let orbit = Orbit { yaw: 0.0, pitch: 0.0, zoom: 1.0 }.turned(*yaw, pitch).zoomed(zoom);
                let view = View { orbit, style, center: bounds.center(), radius: bounds.radius(), background, line };
                let pixels = capture.render(&gpu, &target, &view).map_err(message)?;
                let (x, y) = ((index % columns) as usize * row, (index / columns) as usize * usize::try_from(h).unwrap_or(0));
                for (line, source) in pixels.chunks_exact(row).enumerate() {
                    let at = (y + line).saturating_mul(sheet_row).saturating_add(x);
                    sheet.get_mut(at..at.saturating_add(row)).ok_or("the capture doesn't fit its sheet")?.copy_from_slice(source);
                }
            }
            Ok((sheet_w, sheet_h, sheet))
        })?;
        let raster = Raster::new(width, height, pixels, ImageLimits::default()).map_err(|e| format!("the capture can't be used: {e}"))?;
        self.stage(&raster)
    }
}
