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
        // A function-scope array the GPU's stack can't hold (Metal refused it at compile time).
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { var a: array<vec4<f32>, 100000>; a[0] = color; return a[0]; }", "too large"),
        ("var<workgroup> scratch: array<f32, 64>;\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }", "globals"),
        ("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { var c = color; while (c.r < 1.0) { c.r += 0.1; } return c; }", "loop"),
        ("fn nc_peek() -> f32 { return 0.0; }\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color * nc_peek(); }", "host"),
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

#[test]
fn call_chains_that_multiply_work_are_refused() {
    // f0 is cheap; each fN calls f(N-1) twice: 2^30 calls per pixel from 1.5 KB of source.
    let mut source = String::from("fn f0(x: f32) -> f32 { return x * 1.0001; }\n");
    for n in 1..30 {
        source.push_str(&format!("fn f{n}(x: f32) -> f32 {{ return f{p}(x) + f{p}(x); }}\n", p = n - 1));
    }
    source.push_str("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color * f29(1.0); }");
    let error = validate(&spec(&source)).unwrap_err();
    assert!(error.0.contains("too much work"), "{error}");
    let mut short = String::from("fn f0(x: f32) -> f32 { return x * 1.0001; }\n");
    for n in 1..4 {
        short.push_str(&format!("fn f{n}(x: f32) -> f32 {{ return f{p}(x) + f{p}(x); }}\n", p = n - 1));
    }
    short.push_str("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color * f3(1.0); }");
    validate(&spec(&short)).unwrap();
}

#[test]
fn compiler_reports_point_at_the_persons_line_in_plain_ascii_framing() {
    let error = validate(&spec("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return color;\n}\noops")).unwrap_err().0;
    assert!(error.contains("wgsl:4:1"), "the line is the person's own: {error}");
    assert!(!error.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)), "box-drawing characters many fonts lack: {error}");
}
