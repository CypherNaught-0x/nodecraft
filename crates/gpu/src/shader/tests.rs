use super::*;

fn spec(source: &str) -> ShaderSpec {
    ShaderSpec { source: source.into(), params: vec![ShaderParam { name: "strength".into(), label: "Strength".into(), min: 0.0, max: 1.0, default: 0.5 }] }
}

const INVERT: &str = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(mix(color.rgb, vec3<f32>(1.0) - color.rgb, strength()), color.a);
}";

#[test]
fn a_per_pixel_effect_with_its_parameter_validates() {
    let valid = validate(&spec(INVERT)).unwrap();
    assert!(valid.module().contains("fn strength() -> f32"));
    // Neighbours through the helper are fine.
    let neighbour = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
        let p = vec2<i32>(uv * size());
        return (pixel(p.x - 1, p.y) + color + pixel(p.x + 1, p.y)) / 3.0;
    }";
    validate(&spec(neighbour)).unwrap();
}

#[test]
fn unsafe_or_broken_shaders_are_refused_with_a_reason() {
    let cases = [
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { loop { } return color; }", "loop"),
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { var c = color; for (var i = 0; i < 4; i++) { c = c * 0.9; } return c; }", "loop"),
        (
            "@group(0) @binding(3) var<storage, read_write> leak: array<u32>;\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }",
            "bindings",
        ),
        ("var<private> state: f32;\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }", "globals"),
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { nc_dst[0] = 0u; return color; }", "host buffer"),
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color", ""),
        ("fn other(c: vec4<f32>) -> vec4<f32> { return c; }", ""),
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }\n@compute @workgroup_size(1) fn extra() {}", ""),
        ("override k: f32 = 1.0;\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color * k; }", "overrides"),
    ];
    for (source, why) in cases {
        let error = validate(&spec(source)).expect_err(source);
        assert!(error.0.contains(why), "{source}: {error}");
    }
    let mut bad = spec(INVERT);
    bad.params[0].name = "main".into();
    assert!(validate(&bad).is_err());
    bad.params[0].name = "strength".into();
    bad.params[0].min = 2.0;
    assert!(validate(&bad).is_err());
    let mut many = spec(INVERT);
    many.params = (0..17).map(|i| ShaderParam { name: format!("p{i}"), label: "P".into(), min: 0.0, max: 1.0, default: 0.0 }).collect();
    assert!(validate(&many).is_err());
    assert!(validate(&spec(&"x".repeat(MAX_SOURCE + 1))).is_err());
}

#[test]
#[ignore = "requires an actual GPU; run explicitly"]
fn a_shader_runs_on_the_gpu() {
    let valid = validate(&spec(INVERT)).unwrap();
    let mut runner = ShaderRunner::request().unwrap();
    let rgba = [255u8, 0, 0, 255, 0, 255, 0, 128];
    let out = runner.run(&valid, &[1.0], 2, 1, &rgba).unwrap();
    assert_eq!(out, vec![0, 255, 255, 255, 255, 0, 255, 128]);
    let half = runner.run(&valid, &[0.0], 2, 1, &rgba).unwrap();
    assert_eq!(half, rgba.to_vec(), "strength 0 leaves it alone");
}
