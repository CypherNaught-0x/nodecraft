use super::*;

const BLUR: &str = "Here it is:\n\n```wgsl\n// param: radius | Radius | 0 | 4 | 1\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return color;\n}\n```\nEnjoy.";

#[test]
fn the_fenced_block_is_the_shader_and_its_param_lines_its_sliders() {
    let shader = parse_reply(BLUR).unwrap();
    assert!(shader.source.starts_with("fn effect"), "the declaration isn't kept in the source: {}", shader.source);
    assert!(shader.source.ends_with("}\n"), "the prose after the block is left out: {}", shader.source);
    assert!(!shader.source.contains("Enjoy"));
    assert_eq!(shader.params, [StoredParam { name: "radius".into(), label: "Radius".into(), min: 0.0, max: 4.0, default: 1.0 }]);
    nodecraft_gpu::shader::validate(&crate::shader_cmds::spec(&shader)).unwrap();
}

#[test]
fn an_answer_without_fences_is_read_whole() {
    let shader = parse_reply("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }").unwrap();
    assert!(shader.params.is_empty());
    assert!(shader.source.starts_with("fn effect"));
}

#[test]
fn malformed_answers_say_what_is_wrong() {
    assert!(parse_reply("").unwrap_err().contains("no shader"));
    assert!(parse_reply("```wgsl\n```").unwrap_err().contains("no shader"));
    assert!(parse_reply("// param: radius | Radius | 0 | 4").unwrap_err().contains("must read"));
    assert!(parse_reply("// param: radius | Radius | 0 | 4 | 1").unwrap_err().contains("only parameter lines"));
    assert!(parse_reply("// param: radius | Radius | 0 | wide | 1").unwrap_err().contains("max isn't a number"));
    assert!(parse_reply("// param: radius | Radius | 0 | inf | 1").unwrap_err().contains("isn't a number"));
    assert!(parse_reply("// param: radius | Radius | NaN | 1 | 1").unwrap_err().contains("isn't a number"));
    let many: String = (0..=nodecraft_gpu::shader::MAX_PARAMS).map(|n| format!("// param: p{n} | P | 0 | 1 | 0\n")).collect();
    assert!(parse_reply(&format!("{many}fn effect")).unwrap_err().contains("more than 16"));
    assert!(parse_reply(&"a".repeat(MAX_REPLY_BYTES + 1)).unwrap_err().contains("too long"));
}

#[test]
fn hostile_fences_never_panic() {
    for reply in ["```", "```wgsl", "```\n", "x```é\n```", "```é", "é```\né", "```wgsl\nfn", "``` ```"] {
        let _ = parse_reply(reply);
    }
}

#[test]
fn the_prompt_carries_the_contract_the_shader_the_message_and_the_request() {
    let current = StoredShader::default();
    let prompt = compose(&current, Some("make it sepia"), Some("line 2: unknown identifier `tint`"));
    assert!(prompt.starts_with(CONTRACT));
    assert!(prompt.contains("```wgsl\nfn effect"), "the source's lines keep their numbers: {prompt}");
    assert!(prompt.contains("}\n// param: amount | Amount | 0 | 1 | 1\n```"), "{prompt}");
    assert!(prompt.contains("unknown identifier `tint`"));
    assert!(prompt.ends_with("make it sepia\n"));
    let fix = compose(&current, None, Some("bad"));
    assert!(fix.contains("change only what the message asks for"));
    // The shader shown to the model reads back as the same shader.
    assert_eq!(parse_reply(&fix).unwrap(), current);
}
