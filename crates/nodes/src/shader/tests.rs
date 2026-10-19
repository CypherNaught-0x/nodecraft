use super::*;
use nodecraft_types::{ErrorKind, NodeId};

fn boundary_shader() -> StoredShader {
    let mut shader = StoredShader {
        source: "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> { return color; }".into(),
        params: (0..16).map(|i| StoredParam { name: format!("p{i}"), label: "P".into(), min: 0.0, max: 1.0, default: 0.0 }).collect(),
    };
    let padding = MAX_CONFIG_BYTES - serde_json::to_string(&shader).unwrap().len();
    shader.source = "\t".repeat(padding / 2) + &" ".repeat(padding % 2) + &shader.source;
    assert!(shader.source.len() <= 32 * 1024, "the accepted boundary retains the source cap");
    assert_eq!(serde_json::to_string(&shader).unwrap().len(), MAX_CONFIG_BYTES);
    shader
}

#[test]
fn encoded_shader_at_the_limit_round_trips() {
    let shader = boundary_shader();
    let value = config_value(&shader).unwrap();
    assert!(matches!(&value, Value::Text(json) if json.len() == MAX_CONFIG_BYTES));
    let mut node = Node::new(NodeId::from_bytes([1; 16]), ID);
    node.config.insert(CONFIG.into(), value);
    assert_eq!(stored(&node).unwrap(), shader);
}

#[test]
fn writer_and_reader_refuse_one_encoded_byte_over_the_limit() {
    let mut shader = boundary_shader();
    shader.source.push(' ');
    let encoded = serde_json::to_string(&shader).unwrap();
    assert_eq!(encoded.len(), MAX_CONFIG_BYTES + 1);
    let error = config_value(&shader).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("64 KiB"));
    let mut node = Node::new(NodeId::from_bytes([1; 16]), ID);
    node.config.insert(CONFIG.into(), Value::Text(encoded));
    assert!(stored(&node).is_err(), "hostile persisted input keeps the same cap");
}
