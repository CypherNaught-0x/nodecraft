//! Private borrowed JSON admission; no owned JSON tree or generation history is constructed.
use super::*;
use serde::ser::{self, SerializeMap, SerializeSeq};
#[derive(Debug)]
pub(super) struct Budget;
impl std::fmt::Display for Budget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("selection is too large to copy and paste back in one go")
    }
}
impl std::error::Error for Budget {}
impl ser::Error for Budget {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
}
#[derive(Default)]
struct Meter {
    items: usize,
    bytes: usize,
    depth: usize,
    key: bool,
}
impl Meter {
    fn item(&mut self) -> Result<(), Budget> {
        if self.key {
            return Err(Budget);
        }
        self.items = self.items.checked_add(1).ok_or(Budget)?;
        if self.items > 16384 || self.depth > 64 { Err(Budget) } else { Ok(()) }
    }
    fn text(&mut self, s: &str) -> Result<(), Budget> {
        self.bytes = self.bytes.checked_add(s.len()).ok_or(Budget)?;
        if self.bytes > 1024 * 1024 { Err(Budget) } else { Ok(()) }
    }
    fn container(&mut self, len: Option<usize>) -> Result<Compound<'_>, Budget> {
        self.item()?;
        if len.is_some_and(|n| n > 16384) {
            return Err(Budget);
        }
        self.depth = self.depth.checked_add(1).ok_or(Budget)?;
        Ok(Compound { meter: self, close: 1 })
    }
}
struct Compound<'a> {
    meter: &'a mut Meter,
    close: usize,
}
impl Compound<'_> {
    fn end(self) -> Result<(), Budget> {
        self.meter.depth = self.meter.depth.saturating_sub(self.close);
        Ok(())
    }
}
impl<'a> ser::Serializer for &'a mut Meter {
    type Ok = ();
    type Error = Budget;
    type SerializeSeq = Compound<'a>;
    type SerializeTuple = Compound<'a>;
    type SerializeTupleStruct = Compound<'a>;
    type SerializeTupleVariant = Compound<'a>;
    type SerializeMap = Compound<'a>;
    type SerializeStruct = Compound<'a>;
    type SerializeStructVariant = Compound<'a>;
    fn serialize_bool(self, _: bool) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_i8(self, _: i8) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_i16(self, _: i16) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_i32(self, _: i32) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_i64(self, _: i64) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_i128(self, _: i128) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_u8(self, _: u8) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_u16(self, _: u16) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_u32(self, _: u32) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_u64(self, _: u64) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_u128(self, _: u128) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_f32(self, _: f32) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_f64(self, _: f64) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_char(self, v: char) -> Result<(), Budget> {
        let mut b = [0; 4];
        self.serialize_str(v.encode_utf8(&mut b))
    }
    fn serialize_str(self, v: &str) -> Result<(), Budget> {
        if !self.key {
            self.item()?;
        }
        self.text(v)
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Budget> {
        let mut c = self.container(Some(v.len()))?;
        for x in v {
            SerializeSeq::serialize_element(&mut c, x)?;
        }
        c.end()
    }
    fn serialize_none(self) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_some<T: Serialize + ?Sized>(self, v: &T) -> Result<(), Budget> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Budget> {
        self.item()
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, v: &'static str) -> Result<(), Budget> {
        self.serialize_str(v)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _: &'static str, v: &T) -> Result<(), Budget> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(self, _: &'static str, _: u32, k: &'static str, v: &T) -> Result<(), Budget> {
        let mut c = self.container(Some(1))?;
        SerializeMap::serialize_entry(&mut c, k, v)?;
        c.end()
    }
    fn serialize_seq(self, n: Option<usize>) -> Result<Compound<'a>, Budget> {
        self.container(n)
    }
    fn serialize_tuple(self, n: usize) -> Result<Compound<'a>, Budget> {
        self.container(Some(n))
    }
    fn serialize_tuple_struct(self, _: &'static str, n: usize) -> Result<Compound<'a>, Budget> {
        self.container(Some(n))
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, k: &'static str, n: usize) -> Result<Compound<'a>, Budget> {
        self.item()?;
        self.depth = self.depth.checked_add(1).ok_or(Budget)?;
        self.text(k)?;
        let mut c = self.container(Some(n))?;
        c.close = 2;
        Ok(c)
    }
    fn serialize_map(self, n: Option<usize>) -> Result<Compound<'a>, Budget> {
        self.container(n)
    }
    fn serialize_struct(self, _: &'static str, n: usize) -> Result<Compound<'a>, Budget> {
        self.container(Some(n))
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, k: &'static str, n: usize) -> Result<Compound<'a>, Budget> {
        self.item()?;
        self.depth = self.depth.checked_add(1).ok_or(Budget)?;
        self.text(k)?;
        let mut c = self.container(Some(n))?;
        c.close = 2;
        Ok(c)
    }
    fn collect_str<T: std::fmt::Display + ?Sized>(self, value: &T) -> Result<(), Budget> {
        struct Count(usize);
        impl std::fmt::Write for Count {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                self.0 = self.0.checked_add(s.len()).filter(|n| *n <= 1024 * 1024).ok_or(std::fmt::Error)?;
                Ok(())
            }
        }
        use std::fmt::Write;
        let mut count = Count(0);
        write!(&mut count, "{value}").map_err(|_| Budget)?;
        if !self.key {
            self.item()?;
        }
        self.bytes = self.bytes.checked_add(count.0).filter(|n| *n <= 1024 * 1024).ok_or(Budget)?;
        Ok(())
    }
}
impl ser::SerializeSeq for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Budget> {
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}
impl ser::SerializeTuple for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Budget> {
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}
impl ser::SerializeTupleStruct for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Budget> {
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}
impl ser::SerializeTupleVariant for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Budget> {
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}
impl ser::SerializeMap for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Budget> {
        self.meter.key = true;
        let result = v.serialize(&mut *self.meter);
        self.meter.key = false;
        result
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, v: &T) -> Result<(), Budget> {
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}
impl ser::SerializeStruct for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, k: &'static str, v: &T) -> Result<(), Budget> {
        self.meter.text(k)?;
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}
impl ser::SerializeStructVariant for Compound<'_> {
    type Ok = ();
    type Error = Budget;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, k: &'static str, v: &T) -> Result<(), Budget> {
        self.meter.text(k)?;
        v.serialize(&mut *self.meter)
    }
    fn end(self) -> Result<(), Budget> {
        self.end()
    }
}

#[derive(Serialize)]
struct BorrowedNode<'a> {
    id: &'a NodeId,
    kind: &'a str,
    title: &'a str,
    pos: &'a Position,
    params: &'a BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    config: &'a BTreeMap<String, Value>,
    inputs: &'a [nodecraft_graph::Port],
    outputs: &'a [nodecraft_graph::Port],
    history: &'a [nodecraft_graph::Generation],
    active: Option<nodecraft_types::GenerationId>,
    tag: Option<nodecraft_types::TagId>,
    field: Option<nodecraft_types::FieldId>,
}
impl<'a> From<&'a Node> for BorrowedNode<'a> {
    fn from(node: &'a Node) -> Self {
        let Node { id, kind, title, pos, params, config, inputs, outputs, history: _, active: _, tag, field } = node;
        Self { id, kind, title, pos, params, config, inputs, outputs, history: &[], active: None, tag: *tag, field: *field }
    }
}
struct Nodes<'a>(&'a Canvas, &'a Selection);
impl Serialize for Nodes<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out = s.serialize_seq(None)?;
        for id in self.1.nodes() {
            if let Some(node) = self.0.nodes().get(id) {
                out.serialize_element(&BorrowedNode::from(node))?;
            }
        }
        out.end()
    }
}
struct Edges<'a>(&'a Canvas, &'a Selection);
impl Serialize for Edges<'_> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out = s.serialize_seq(None)?;
        for edge in self.0.edges().values().filter(|e| self.1.contains(e.from.node) && self.1.contains(e.to.node)) {
            out.serialize_element(edge)?;
        }
        out.end()
    }
}
#[derive(Serialize)]
struct Command<'a> {
    canvas: CanvasId,
    nodes: Nodes<'a>,
    edges: Edges<'a>,
}
#[derive(Serialize)]
struct Envelope<'a> {
    format: &'static str,
    version: u32,
    project: ProjectId,
    nodes: Nodes<'a>,
    edges: Edges<'a>,
}
struct Wire {
    bytes: usize,
    limit: usize,
}
impl std::io::Write for Wire {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.checked_add(b.len()).filter(|n| *n <= self.limit).ok_or_else(|| std::io::Error::other("clipboard limit"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn check(project: ProjectId, canvas: &Canvas, selection: &Selection) -> Result<(), Budget> {
    let mut meter = Meter { depth: 1, ..Default::default() };
    Command { canvas: canvas.id(), nodes: Nodes(canvas, selection), edges: Edges(canvas, selection) }.serialize(&mut meter)?;
    serde_json::to_writer(
        Wire { bytes: 0, limit: MAX_BYTES },
        &Envelope { format: FORMAT, version: VERSION, project, nodes: Nodes(canvas, selection), edges: Edges(canvas, selection) },
    )
    .map_err(|_| Budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value as Json, json};
    fn admitted(value: &impl Serialize) -> bool {
        value.serialize(&mut Meter { depth: 1, ..Default::default() }).is_ok()
    }
    #[test]
    fn logical_meter_matches_actual_command_boundaries_and_empty_depth() {
        for bytes in [1024 * 1024 - 2, 1024 * 1024 - 1, 1024 * 1024] {
            let value = json!({"k":"x".repeat(bytes)});
            assert_eq!(admitted(&value), nodecraft_engine::commands::validate_json(&value).is_ok());
        }
        for count in [16382, 16383, 16384] {
            let value = Json::Array(vec![Json::Null; count]);
            assert_eq!(admitted(&value), nodecraft_engine::commands::validate_json(&value).is_ok());
        }
        for leaf in [Json::Null, json!([]), json!({})] {
            let mut value = leaf;
            for _ in 0..70 {
                assert_eq!(admitted(&value), nodecraft_engine::commands::validate_json(&value).is_ok());
                value = Json::Array(vec![value]);
            }
        }
        for n in 0..512 {
            let value = json!({format!("key{n}é"): [{"control\0":"\n\r\t\"\\é".repeat(n%31)},[],{},n%7==0],"media":{"type":"media","value":{"hash":"00".repeat(32),"mime":"text/plain","meta":{"width":1,"duration_ms":2}}}});
            assert_eq!(admitted(&value), nodecraft_engine::commands::validate_json(&value).is_ok());
        }
    }
    #[test]
    fn wire_counter_matches_actual_escaping_and_exact_limit() {
        let controls: String = (0u8..32).map(char::from).collect();
        let value = json!({"key":controls+"é\"\\"});
        let bytes = serde_json::to_vec(&value).unwrap();
        let mut exact = Wire { bytes: 0, limit: bytes.len() };
        serde_json::to_writer(&mut exact, &value).unwrap();
        assert_eq!(exact.bytes, bytes.len());
        assert!(serde_json::to_writer(Wire { bytes: 0, limit: bytes.len() - 1 }, &value).is_err());
    }
    #[test]
    fn projection_preserves_every_retained_field_and_ignores_large_history() {
        use nodecraft_graph::{Change, Generation, Port};
        use nodecraft_types::{GenerationId, Modality, PortType};
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Projection").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(NodeId::from_bytes([3; 16]), "unknown.plugin");
        node.title = "Escaped \0 é".into();
        node.pos = Position { x: 12.5, y: -40.0 };
        node.params.insert("list".into(), Value::List(vec![Value::Text("retained".into())]));
        node.config.insert("setting".into(), Value::Bool(true));
        node.outputs.push(Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } });
        for n in 10u8..18 {
            node.history.push(Generation::new(GenerationId::from_bytes([n; 16]), vec![Value::Text("h".repeat(512 * 1024))]));
        }
        node.active = Some(GenerationId::from_bytes([10; 16]));
        let borrowed = serde_json::to_value(BorrowedNode::from(&node)).unwrap();
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
        let mut selection = Selection::default();
        selection.only(NodeId::from_bytes([3; 16]));
        PROJECTED_NODES.with(|n| n.set(0));
        let result = payload(project.id(), project.canvases().get(&canvas).unwrap(), &selection).unwrap();
        assert_eq!(PROJECTED_NODES.with(|n| n.get()), 1);
        let copied = result.nodes.first().unwrap();
        assert!(copied.history.is_empty() && copied.active.is_none());
        assert_eq!(serde_json::to_value(copied).unwrap(), borrowed);
        let command = json!({"canvas":canvas,"nodes":result.nodes,"edges":result.edges});
        nodecraft_engine::commands::validate_json(&command).unwrap();
    }
}
