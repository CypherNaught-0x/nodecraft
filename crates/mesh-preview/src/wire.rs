use crate::{BUILD, Error, Layout, PROTOCOL};
use js_sys::{Array, ArrayBuffer, Function, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Job {
    pub generation: u32,
    pub sequence: u32,
    pub hash: [u8; 32],
    pub length: u32,
}
pub(crate) fn text(value: &JsValue, cap: u32) -> Result<String, Error> {
    if !value.is_string() {
        return Err(Error::Protocol);
    }
    // Boxing preserves the primitive without copying its body. Read the raw numeric
    // length before any u32 accessor or Rust string allocation can truncate it.
    let object: Function = Reflect::get(&js_sys::global(), &"Object".into()).map_err(|_| Error::Protocol)?.dyn_into().map_err(|_| Error::Protocol)?;
    let boxed = object.call1(&JsValue::UNDEFINED, value).map_err(|_| Error::Protocol)?;
    number(&Reflect::get(&boxed, &"length".into()).map_err(|_| Error::Protocol)?, cap)?;
    value.as_string().ok_or(Error::Protocol)
}
pub(crate) fn number(value: &JsValue, maximum: u32) -> Result<u32, Error> {
    let n = value.as_f64().ok_or(Error::Protocol)?;
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > f64::from(maximum) {
        return Err(Error::Protocol);
    }
    Ok(n as u32)
}
pub(crate) fn arity(value: &JsValue, maximum: u32) -> Result<u32, Error> {
    if !Array::is_array(value) {
        return Err(Error::Protocol);
    }
    let raw = Reflect::get(value, &"length".into()).map_err(|_| Error::Protocol)?;
    number(&raw, maximum)
}
pub(crate) fn array(value: &JsValue, length: u32) -> Result<Array, Error> {
    if arity(value, length)? != length {
        return Err(Error::Protocol);
    }
    Ok(value.clone().unchecked_into())
}
pub(crate) fn header(kind: &str) -> Array {
    let out = Array::new();
    out.push(&kind.into());
    out.push(&PROTOCOL.into());
    out.push(&BUILD.into());
    out
}
pub(crate) fn check_header(array: &Array, kind: &str) -> Result<(), Error> {
    if text(&array.get(0), 16)? != kind || number(&array.get(1), PROTOCOL)? != PROTOCOL || text(&array.get(2), 160)? != BUILD {
        return Err(Error::Protocol);
    }
    Ok(())
}
pub(crate) fn append_job(out: &Array, job: Job) {
    out.push(&job.generation.into());
    out.push(&job.sequence.into());
    out.push(&blake3::Hash::from_bytes(job.hash).to_hex().to_string().into());
    out.push(&job.length.into());
}
pub(crate) fn job(array: &Array) -> Result<Job, Error> {
    let encoded = text(&array.get(5), 64)?;
    let hash = blake3::Hash::from_hex(encoded).map_err(|_| Error::Protocol)?;
    Ok(Job {
        generation: number(&array.get(3), u32::MAX)?,
        sequence: number(&array.get(4), u32::MAX)?,
        hash: *hash.as_bytes(),
        length: number(&array.get(6), crate::MAX_INPUT as u32)?,
    })
}
pub(crate) fn buffer(value: &JsValue, maximum: usize) -> Result<ArrayBuffer, Error> {
    // Get the raw numeric property BEFORE web-sys/js-sys's u32-returning accessor can truncate.
    if !value.is_instance_of::<ArrayBuffer>() {
        return Err(Error::Protocol);
    }
    let size = Reflect::get(value, &"byteLength".into()).map_err(|_| Error::Protocol)?;
    number(&size, u32::try_from(maximum).map_err(|_| Error::TooLarge)?)?;
    let buffer: ArrayBuffer = value.clone().dyn_into().map_err(|_| Error::Protocol)?;
    let actual = view(&buffer)?;
    let actual_size = Reflect::get(&actual, &"byteLength".into()).map_err(|_| Error::Protocol)?;
    let actual_size = number(&actual_size, u32::try_from(maximum).map_err(|_| Error::TooLarge)?)?;
    if f64::from(actual_size) != size.as_f64().ok_or(Error::Protocol)? {
        return Err(Error::Protocol);
    }
    // Transfer buffers created here are fixed-size. Do not retain a resizable incoming body.
    if Reflect::get(value, &"resizable".into()).map_err(|_| Error::Protocol)?.as_bool() == Some(true) {
        return Err(Error::Protocol);
    }
    Ok(buffer)
}
pub(crate) fn owned(bytes: &[u8]) -> Result<ArrayBuffer, Error> {
    let size = u32::try_from(bytes.len()).map_err(|_| Error::TooLarge)?;
    let buffer = allocate(size)?;
    view(&buffer)?.copy_from(bytes);
    Ok(buffer)
}
pub(crate) fn copy(buffer: &ArrayBuffer, maximum: usize) -> Result<Vec<u8>, Error> {
    let raw = Reflect::get(buffer, &"byteLength".into()).map_err(|_| Error::Protocol)?;
    let length = usize::try_from(number(&raw, u32::try_from(maximum).map_err(|_| Error::TooLarge)?)?).map_err(|_| Error::TooLarge)?;
    let view = view(buffer)?;
    if usize::try_from(view.length()).map_err(|_| Error::TooLarge)? != length {
        return Err(Error::Protocol);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(length).map_err(|_| Error::Capacity)?;
    out.resize(length, 0);
    view.copy_to(&mut out);
    Ok(out)
}
pub(crate) fn bounds(value: &JsValue) -> Result<nodecraft_media::Bounds, Error> {
    let data = array(value, 6)?;
    let mut out = [0.0; 6];
    for (i, v) in out.iter_mut().enumerate() {
        let raw = data.get(u32::try_from(i).map_err(|_| Error::Protocol)?).as_f64().ok_or(Error::Protocol)?;
        if !raw.is_finite() || raw.abs() > f64::from(f32::MAX) {
            return Err(Error::Invalid);
        }
        *v = raw as f32;
    }
    let [a, b, c, d, e, f] = out;
    Ok(nodecraft_media::Bounds { min: [a, b, c], max: [d, e, f] })
}
pub(crate) fn layout(data: &Array) -> Result<Layout, Error> {
    let out = Layout { vertices: number(&data.get(7), 600_000)?, triangles: number(&data.get(8), 300_000)?, edge_words: number(&data.get(9), 1_800_000)? };
    out.sizes(crate::Profile::Browser)?;
    Ok(out)
}

pub(crate) fn allocate(length: u32) -> Result<ArrayBuffer, Error> {
    let constructor =
        Reflect::get(&js_sys::global(), &"ArrayBuffer".into()).map_err(|_| Error::Unavailable)?.dyn_into::<Function>().map_err(|_| Error::Unavailable)?;
    let args = Array::new();
    args.push(&length.into());
    Reflect::construct(&constructor, &args).map_err(|_| Error::Capacity)?.dyn_into().map_err(|_| Error::Protocol)
}
pub(crate) fn view(buffer: &ArrayBuffer) -> Result<Uint8Array, Error> {
    let constructor =
        Reflect::get(&js_sys::global(), &"Uint8Array".into()).map_err(|_| Error::Unavailable)?.dyn_into::<Function>().map_err(|_| Error::Unavailable)?;
    let args = Array::new();
    args.push(buffer);
    Reflect::construct(&constructor, &args).map_err(|_| Error::Capacity)?.dyn_into().map_err(|_| Error::Protocol)
}
