//! Custom WGSL shader actions (#90): a person (or, later, a model) writes one function,
//!
//! ```wgsl
//! fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { ... }
//! ```
//!
//! with up to 16 declared float parameters, each readable as a function of its name
//! (`strength()`), plus two host helpers: `pixel(x, y)` (the source pixel, clamped to the
//! edges) and `size()` (the image size in pixels). The host wraps it in its own compute shader,
//! which reads the source image, calls `effect` once per pixel (colours are 0–1 RGBA) and writes
//! the clamped result.
//!
//! **Validated before it reaches a GPU** ([`validate`]), with naga: the assembled module must
//! parse and validate, and the person's code must stay a pure per-pixel function. It may declare
//! no bindings, globals or overrides, touch no host buffer except through the helpers, contain no
//! loop and no barrier; no value may exceed 16 KiB; and a static cost (every operation, with
//! each call counted at its callee's full cost) is bounded per pixel, so call chains can't
//! multiply the work. The source is bounded, and so are the parameters. A shader that passes
//! does bounded work per pixel, but a large image can still take long: the runner gives up after
//! a timeout and starts afresh.
use naga::{ArraySize, Block, Expression, Handle, Statement, TypeInner};
use std::collections::BTreeMap;

/// Longest shader source.
pub const MAX_SOURCE: usize = 32 * 1024;
/// Most declared parameters.
pub const MAX_PARAMS: usize = 16;
/// Largest value a shader may hold (a function-scope array, say): beyond this, GPU stacks run out.
pub const MAX_VALUE_BYTES: u32 = 16 * 1024;
/// Most operations one pixel may cost, counting every call through its callee's cost.
pub const MAX_COST: u64 = 100_000;

/// A declared float parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderParam {
    pub name: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

/// A shader action as written.
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderSpec {
    pub source: String,
    pub params: Vec<ShaderParam>,
}

/// A spec that passed [`validate`]: the full module the host runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidShader {
    module: String,
    params: usize,
}

impl ValidShader {
    /// The WGSL the host compiles.
    pub fn module(&self) -> &str {
        &self.module
    }
}

/// Why a shader was refused, in words for the person who wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderError(pub String);

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ShaderError {}

fn refuse<T>(why: impl Into<String>) -> Result<T, ShaderError> {
    Err(ShaderError(why.into()))
}

/// Functions the host provides; the person's functions may not use these names.
const HOST: [&str; 4] = ["main", "pixel", "size", "nc_param"];

const PRELUDE: &str = "
struct NcParams { size: vec4<u32>, values: array<vec4<f32>, 4>, }
@group(0) @binding(0) var<storage, read> nc_src: array<u32>;
@group(0) @binding(1) var<storage, read_write> nc_dst: array<u32>;
@group(0) @binding(2) var<uniform> nc_params: NcParams;
fn nc_param(i: u32) -> f32 { return nc_params.values[i / 4u][i % 4u]; }
fn size() -> vec2<f32> { return vec2<f32>(f32(nc_params.size.x), f32(nc_params.size.y)); }
fn pixel(x: i32, y: i32) -> vec4<f32> {
    let w = i32(nc_params.size.x);
    let h = i32(nc_params.size.y);
    let cx = clamp(x, 0, w - 1);
    let cy = clamp(y, 0, h - 1);
    return unpack4x8unorm(nc_src[u32(cy * w + cx)]);
}
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= nc_params.size.x || id.y >= nc_params.size.y) { return; }
    let i = id.y * nc_params.size.x + id.x;
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / size();
    let out = clamp(effect(unpack4x8unorm(nc_src[i]), uv), vec4<f32>(0.0), vec4<f32>(1.0));
    nc_dst[i] = pack4x8unorm(out);
}
";

/// A compiler report with its box-drawing frame in ASCII, which every font and terminal shows.
fn plain(report: &str) -> ShaderError {
    ShaderError(
        report
            .chars()
            .map(|c| match c {
                '─' | '━' | '═' | '┄' | '┅' | '┈' | '┉' | '╌' | '╍' => '-',
                '│' | '┃' | '║' | '┆' | '┇' | '┊' | '┋' | '╎' | '╏' => '|',
                '\u{2500}'..='\u{257f}' => '+',
                c => c,
            })
            .collect(),
    )
}

fn valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z')) && name.len() <= 32 && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Checks `spec` and assembles the module the host runs.
pub fn validate(spec: &ShaderSpec) -> Result<ValidShader, ShaderError> {
    if spec.source.len() > MAX_SOURCE {
        return refuse(format!("a shader is at most {} KiB", MAX_SOURCE / 1024));
    }
    if spec.params.len() > MAX_PARAMS {
        return refuse(format!("a shader has at most {MAX_PARAMS} parameters"));
    }
    let mut names = std::collections::BTreeSet::new();
    let mut accessors = String::new();
    for (i, p) in spec.params.iter().enumerate() {
        if !valid_name(&p.name) || p.name.starts_with("nc_") || HOST.contains(&p.name.as_str()) || p.name == "effect" {
            return refuse(format!("parameter {:?} needs a lowercase name (letters, digits, _) that isn't taken", p.name.chars().take(40).collect::<String>()));
        }
        if !names.insert(p.name.as_str()) {
            return refuse(format!("parameter {} is declared twice", p.name));
        }
        if !(p.min.is_finite() && p.max.is_finite() && p.default.is_finite()) || p.min >= p.max || !(p.min..=p.max).contains(&p.default) {
            return refuse(format!("parameter {} needs finite min < max with its default between them", p.name));
        }
        if p.label.trim().is_empty() || p.label.len() > 64 || p.label.chars().any(char::is_control) {
            return refuse(format!("parameter {} needs a short label", p.name));
        }
        accessors.push_str(&format!("fn {}() -> f32 {{ return nc_param({i}u); }}\n", p.name));
    }
    // The person's source comes first, so the line numbers in errors are theirs.
    let module_text = format!("{}\n{accessors}{PRELUDE}", spec.source);
    let module = naga::front::wgsl::parse_str(&module_text).map_err(|e| plain(&e.emit_to_string(&module_text)))?;
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
        .validate(&module)
        .map_err(|e| plain(&e.emit_to_string(&module_text)))?;
    if module.global_variables.len() != 3 || !module.overrides.is_empty() {
        return refuse("a shader may not declare bindings, globals or overrides");
    }
    if module.entry_points.len() != 1 || module.entry_points.first().is_none_or(|e| e.name != "main") {
        return refuse("a shader may not declare entry points");
    }
    let host: std::collections::BTreeSet<&str> = HOST.into_iter().chain(spec.params.iter().map(|p| p.name.as_str())).collect();
    let mut effect = false;
    for (_, function) in module.functions.iter() {
        let name = function.name.as_deref().unwrap_or("");
        if host.contains(name) {
            continue;
        }
        if name.starts_with("nc_") {
            return refuse("names starting with nc_ belong to the host");
        }
        if name == "effect" {
            effect = true;
            let args = function.arguments.len();
            if args != 2 || function.result.is_none() {
                return refuse("effect takes (color: vec4<f32>, uv: vec2<f32>) and returns vec4<f32>");
            }
        }
        if function.expressions.iter().any(|(_, e)| matches!(e, Expression::GlobalVariable(_))) {
            return refuse(format!("{name} uses a host buffer; read pixels with pixel(x, y)"));
        }
        check_block(&function.body, name)?;
    }
    if !effect {
        return refuse("a shader defines fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>");
    }
    // Sizes: every fixed-size type within MAX_VALUE_BYTES (the host's buffers are the only
    // runtime-sized arrays, and only host globals may hold them).
    let mut layouter = naga::proc::Layouter::default();
    layouter.update(module.to_ctx()).map_err(|_| ShaderError("a type's size can't be worked out".into()))?;
    for (handle, ty) in module.types.iter() {
        if matches!(ty.inner, TypeInner::Array { size: ArraySize::Dynamic, .. }) {
            continue;
        }
        if layouter[handle].size > MAX_VALUE_BYTES {
            return refuse(format!("a value is too large; values are at most {} KiB", MAX_VALUE_BYTES / 1024));
        }
    }
    // Work: every function's cost with calls counted at their callee's cost (WGSL has no
    // recursion, so the call graph is acyclic).
    let mut costs: BTreeMap<Handle<naga::Function>, u64> = BTreeMap::new();
    let mut order: Vec<Handle<naga::Function>> = module.functions.iter().map(|(h, _)| h).collect();
    // Arena order puts callees before callers in WGSL (no forward calls); iterate to a fixpoint
    // anyway, bounded by the function count.
    for _ in 0..order.len().saturating_add(1) {
        let mut changed = false;
        for &handle in &order {
            let function = &module.functions[handle];
            let cost = function_cost(function, &costs);
            if costs.get(&handle) != Some(&cost) {
                costs.insert(handle, cost);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    order.clear();
    let main = module.entry_points.first().map(|e| function_cost(&e.function, &costs)).unwrap_or(u64::MAX);
    if main > MAX_COST {
        return refuse(format!("the shader does too much work per pixel (about {main} operations; at most {MAX_COST})"));
    }
    Ok(ValidShader { module: module_text, params: spec.params.len() })
}

/// A function's cost: its expressions, plus each call at its callee's cost.
fn function_cost(function: &naga::Function, costs: &BTreeMap<Handle<naga::Function>, u64>) -> u64 {
    fn calls(block: &Block, costs: &BTreeMap<Handle<naga::Function>, u64>) -> u64 {
        let mut total: u64 = 0;
        for statement in block.iter() {
            let add = match statement {
                Statement::Call { function, .. } => costs.get(function).copied().unwrap_or(MAX_COST.saturating_add(1)),
                Statement::Block(inner) => calls(inner, costs),
                Statement::If { accept, reject, .. } => calls(accept, costs).saturating_add(calls(reject, costs)),
                Statement::Switch { cases, .. } => cases.iter().fold(0u64, |sum, case| sum.saturating_add(calls(&case.body, costs))),
                Statement::Loop { body, continuing, .. } => calls(body, costs).saturating_add(calls(continuing, costs)),
                _ => 0,
            };
            total = total.saturating_add(add);
        }
        total
    }
    u64::try_from(function.expressions.len()).unwrap_or(u64::MAX).saturating_add(calls(&function.body, costs))
}

/// No loops (every invocation finishes) and no barriers.
fn check_block(block: &Block, name: &str) -> Result<(), ShaderError> {
    for statement in block.iter() {
        match statement {
            Statement::Loop { .. } => return refuse(format!("{name} has a loop; shaders here run once per pixel without loops")),
            Statement::ControlBarrier(_) | Statement::MemoryBarrier(_) => return refuse(format!("{name} uses a barrier")),
            Statement::Block(inner) => check_block(inner, name)?,
            Statement::If { accept, reject, .. } => {
                check_block(accept, name)?;
                check_block(reject, name)?;
            }
            Statement::Switch { cases, .. } => {
                for case in cases {
                    check_block(&case.body, name)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub use runner::{ShaderRunner, TIMEOUT};

mod runner;

#[cfg(test)]
mod tests;
