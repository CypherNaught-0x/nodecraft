// Copies a rendered 3D preview into egui's pass, filling the viewport egui sets for the callback.

@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
// x: 1 when the output is not an sRGB format, so the shader encodes the colour itself.
@group(0) @binding(2) var<uniform> output: vec4<f32>;

struct BlitOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> BlitOut {
    // One triangle covering the viewport.
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    var out: BlitOut;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

fn encode(linear: vec3<f32>) -> vec3<f32> {
    let low = linear * 12.92;
    let high = 1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(high, low, linear <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(in: BlitOut) -> @location(0) vec4<f32> {
    let color = textureSample(image, image_sampler, in.uv);
    if (output.x > 0.5) {
        return vec4<f32>(encode(color.rgb), color.a);
    }
    return color;
}
