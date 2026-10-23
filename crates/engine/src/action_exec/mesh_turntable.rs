//! Stream one offscreen 3D frame at a time through the existing bounded video encoder.
use super::{LocalActions, number, whole, word};
use nodecraft_nodes::mesh::turntable::Sequence;
use nodecraft_types::{ParamValues, Value};

fn sequence(params: &ParamValues) -> Result<Sequence, String> {
    let clockwise = match word(params, "direction")? {
        "ccw" => false,
        "cw" => true,
        _ => return Err("the direction is clockwise or counter-clockwise".into()),
    };
    let bounce = match word(params, "orbit")? {
        "full" => false,
        "bounce" => true,
        _ => return Err("the orbit is a full turn or back-and-forth".into()),
    };
    Sequence::new(
        [whole(params, "width")?, whole(params, "height")?],
        whole(params, "fps")?,
        number(params, "duration")?,
        number(params, "start")?,
        if bounce { number(params, "end")? } else { 90.0 },
        clockwise,
        bounce,
    )
    .map_err(|e| e.message)
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn frames(sequence: Sequence, cancelled: &dyn Fn() -> bool, mut draw: impl FnMut(u32, f32) -> Result<(), String>) -> Result<(), String> {
    for index in 0..sequence.frames() {
        if cancelled() {
            return Err("the 3D capture was cancelled".into());
        }
        draw(index, sequence.yaw(index).ok_or("the turntable frame is outside its sequence")?)?;
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
impl LocalActions {
    pub(super) fn turntable(&self, params: &ParamValues, _: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
        if cancelled() {
            return Err("the 3D capture was cancelled".into());
        }
        sequence(params)?;
        Err("3D turntable videos need the desktop app and configured ffmpeg and ffprobe".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use crate::action_exec::timeline::native::{message, service, wait};
    use crate::action_exec::video_frames::native::{push, renderer};
    use crate::action_exec::{capture_state, mesh_capture};
    use nodecraft_gpu::mesh::capture::{MeshCapture, TIMEOUT};
    use nodecraft_gpu::mesh::{MeshData, Orbit, Style, View};
    use nodecraft_media::video::RenderSpec;
    use nodecraft_media::{MeshLimits, decode_mesh};
    use std::time::Instant;

    impl LocalActions {
        pub(in crate::action_exec) fn turntable(&self, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
            if cancelled() {
                return Err("the 3D capture was cancelled".into());
            }
            // Refuse aggregate work before opening a service, decoding, allocating a target or requesting a GPU.
            let sequence = sequence(params)?;
            let style = mesh_capture::style(params)?;
            let pitch = number(params, "pitch")?;
            let zoom = number(params, "zoom")?;
            if !(-88.0..=88.0).contains(&pitch) || !(0.25..=16.0).contains(&zoom) {
                return Err("elevation or zoom is outside the 3D viewer's range".into());
            }
            let background = mesh_capture::linear(word(params, "background")?)?;
            let line = mesh_capture::linear(word(params, "line").unwrap_or("#e6e6e6ff"))?;
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to render a 3D turntable")?;
            capture_state::with(cancelled, |slot, closing| {
                // GPU readbacks, queue backpressure and encoded-file readback share one forty-second
                // capture budget, preserving the existing bounded close/drain contract.
                let started = Instant::now();
                let stopping = || closing() || started.elapsed() >= TIMEOUT;
                let result = (|| {
                    let service = service(tools, &stopping)?;
                    let bytes = self.model_bytes(inputs.first())?;
                    let mesh = decode_mesh(&bytes, &MeshLimits::default()).map_err(|e| format!("the input model can't be read: {e}"))?;
                    let bounds = mesh.bounds().ok_or("the input model has nothing to draw")?;
                    let edges = if style == Style::Wireframe { mesh.edges(mesh_capture::MAX_EDGES) } else { None };
                    if stopping() {
                        return Err("the 3D capture was cancelled".into());
                    }
                    *slot = Some(MeshCapture::request().map_err(mesh_capture::message)?);
                    let capture = slot.as_ref().ok_or("3D captures need a GPU, and none is available")?;
                    let gpu = capture
                        .upload(MeshData {
                            positions: &mesh.positions,
                            normals: &mesh.normals,
                            colors: &mesh.colors,
                            indices: &mesh.indices,
                            edges: edges.as_deref(),
                        })
                        .map_err(mesh_capture::message)?;
                    let target = capture.target(sequence.size()).map_err(mesh_capture::message)?;
                    if target.size() != sequence.size() {
                        return Err("this GPU cannot capture the requested turntable size; lower its width or height".into());
                    }
                    let [width, height] = sequence.size();
                    let spec = RenderSpec::new(width, height, (sequence.fps(), 1), sequence.frames(), [0; 3]).map_err(message)?;
                    if stopping() {
                        return Err("the 3D capture was cancelled".into());
                    }
                    let (mut sender, mut render) = renderer(&service, spec, &stopping)?;
                    frames(sequence, &stopping, |index, yaw| {
                        let orbit = Orbit { yaw: 0.0, pitch: 0.0, zoom: 1.0 }.turned(yaw, pitch.to_radians() as f32).zoomed(zoom as f32);
                        let view = View { orbit, style, center: bounds.center(), radius: bounds.radius(), background, line };
                        let pixels = capture.render(&gpu, &target, &view, TIMEOUT.saturating_sub(started.elapsed())).map_err(mesh_capture::message)?;
                        push(&mut sender, &mut render, index, pixels, &stopping)
                    })?;
                    sender.finish().map_err(|_| "not every turntable frame reached the encoder")?;
                    let rendered = wait(render, &stopping).map_err(message)?;
                    self.keep(&service, &rendered, (width, height), sequence.duration_us(), false, &stopping)
                })();
                if result.is_err() && !closing() && started.elapsed() >= TIMEOUT {
                    Err("the 3D turntable took longer than 40 seconds; lower its size, duration or frame rate".into())
                } else {
                    result
                }
            })
        }
    }
}

#[cfg(test)]
mod tests;
