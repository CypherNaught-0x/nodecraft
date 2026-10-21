// The 3D preview (#111): a model in its vertex colours, lit by one light that follows the camera.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // xyz: the light's direction (towards the light); w: 1 draws unlit.
    light: vec4<f32>,
    line_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>, @location(2) color: vec4<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = u.view_proj * vec4<f32>(position, 1.0);
    out.normal = normal;
    out.color = color;
    return out;
}

@fragment
fn fs_shaded(in: VertexOut) -> @location(0) vec4<f32> {
    if (u.light.w > 0.5) {
        return vec4<f32>(in.color.rgb, 1.0);
    }
    var lit = 1.0;
    let length_n = length(in.normal);
    if (length_n > 1e-6) {
        // Two-sided: a model whose faces point inwards still reads.
        lit = 0.3 + 0.7 * abs(dot(in.normal / length_n, normalize(u.light.xyz)));
    }
    return vec4<f32>(in.color.rgb * lit, 1.0);
}

@fragment
fn fs_line(in: VertexOut) -> @location(0) vec4<f32> {
    return u.line_color;
}
