//! Machine-readable builtin parameter contracts, independent of any frontend.
use serde_json::{Value, json};

fn id() -> Value {
    json!({"type":"string", "format":"uuid"})
}
fn position() -> Value {
    json!({"type":"object", "required":["x","y"], "properties":{
    "x":{"type":"number","minimum":-10000000,"maximum":10000000},
    "y":{"type":"number","minimum":-10000000,"maximum":10000000}}, "additionalProperties":false})
}
fn object(properties: Value, required: &[&str]) -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "type":"object", "properties":properties,
        "required":required,"additionalProperties":false})
}
pub fn empty_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "anyOf":[{"type":"null"},
        {"type":"object","properties":{},"additionalProperties":false}]})
}
pub fn remove_schema() -> Value {
    object(json!({"canvas":id(),"id":id()}), &["canvas", "id"])
}
pub fn disconnect_schema() -> Value {
    remove_schema()
}
pub fn move_schema() -> Value {
    object(json!({"canvas":id(),"id":id(),"pos":position(),"gesture":{"type":["string","null"],"minLength":1,"maxLength":128}}), &["canvas", "id", "pos"])
}
fn endpoint() -> Value {
    object(json!({"node":id(),"port":{"type":"string","maxLength":256}}), &["node", "port"])
}
pub fn connect_schema() -> Value {
    let edge =
        object(json!({"id":id(),"from":endpoint(),"to":endpoint(),"order":{"type":"integer","minimum":0,"maximum":u32::MAX}}), &["id", "from", "to", "order"]);
    object(json!({"canvas":id(),"edge":edge}), &["canvas", "edge"])
}
fn port() -> Value {
    object(
        json!({"name":{"type":"string","maxLength":256},"port_type":object(json!({
        "modality":{"enum":["text","image","video","audio","mesh","document"]},"many":{"type":"boolean","default":false}}), &["modality"])}),
        &["name", "port_type"],
    )
}
pub fn add_schema() -> Value {
    let node = object(
        json!({"id":id(),"kind":{"type":"string","minLength":1,"maxLength":256},
        "title":{"type":"string","maxLength":4096},"pos":position(),"params":{"type":"object","additionalProperties":{"$ref":"#/$defs/value"}},
        "inputs":{"type":"array","maxItems":256,"items":port()},"outputs":{"type":"array","maxItems":256,"items":port()},
        "history":{"type":"array","maxItems":0},"active":{"type":"null"},
        "tag":{"type":["string","null"],"format":"uuid"},"field":{"type":["string","null"],"format":"uuid"}}),
        &["id", "kind"],
    );
    let mut schema = object(json!({"canvas":id(),"node":node}), &["canvas", "node"]);
    let definitions = json!({"value":{"oneOf":[
        {"type":"object","required":["type","value"],"properties":{"type":{"const":"text"},"value":{"type":"string","maxLength":1048576}},"additionalProperties":false},
        {"type":"object","required":["type","value"],"properties":{"type":{"const":"list"},"value":{"type":"array","maxItems":4096,"items":{"$ref":"#/$defs/value"}}},"additionalProperties":false},
        {"type":"object","required":["type","value"],"properties":{"type":{"const":"media"},"value":{"type":"object","required":["hash","mime"],"properties":{
            "hash":{"type":"string","pattern":"^[0-9a-f]{64}$"},"mime":{"type":"string"},"meta":{"type":"object"}},"additionalProperties":false}},"additionalProperties":false}
    ]}});
    if let Some(object) = schema.as_object_mut() {
        object.insert("$defs".into(), definitions);
    }
    schema
}
