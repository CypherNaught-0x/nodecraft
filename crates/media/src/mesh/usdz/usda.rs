//! Closed static triangle grammar. Both passes borrow tokens; there is no general USD resolver.
use super::invalid;
use crate::{Bounds, MediaError, Mesh, MeshLimits};
use std::fmt::{self, Write};

const MAX_LINE: usize = 1024;
const MAX_TOKEN: usize = 64;
const FIELDS: u16 = 255;
#[derive(Clone, Copy)]
struct Lexer<'a> {
    text: &'a str,
    at: usize,
}
impl<'a> Lexer<'a> {
    fn next(&mut self) -> Result<Option<&'a str>, MediaError> {
        let bytes = self.text.as_bytes();
        while let Some(b) = bytes.get(self.at) {
            if b.is_ascii_whitespace() {
                self.at += 1;
            } else if *b == b'#' {
                while bytes.get(self.at).is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
                    self.at += 1;
                }
            } else {
                break;
            }
        }
        let Some(&b) = bytes.get(self.at) else { return Ok(None) };
        let start = self.at;
        self.at += 1;
        if b == b'"' {
            let mut closed = false;
            while let Some(&c) = bytes.get(self.at) {
                self.at += 1;
                if c == b'"' {
                    closed = true;
                    break;
                }
                if !c.is_ascii_alphanumeric() && !matches!(c, b'_' | b'-') {
                    return Err(invalid("USDZ quoted tokens require plain ASCII identifiers"));
                }
                if self.at - start > MAX_TOKEN {
                    return Err(invalid("USDZ token exceeds 64 bytes"));
                }
            }
            if !closed {
                return Err(invalid("unterminated USDZ quoted token"));
            }
        } else if !b"(){}[]=,".contains(&b) {
            if !b.is_ascii_alphanumeric() && !matches!(b, b'_' | b'-' | b'+' | b'.' | b':') {
                return Err(invalid("unsupported USDZ token"));
            }
            while bytes.get(self.at).is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.' | b':')) {
                self.at += 1;
            }
        }
        if self.at - start > MAX_TOKEN {
            return Err(invalid("USDZ token exceeds 64 bytes"));
        }
        self.text.get(start..self.at).map(Some).ok_or_else(|| invalid("invalid USDZ token extent"))
    }
    fn required(&mut self) -> Result<&'a str, MediaError> {
        self.next()?.ok_or_else(|| invalid("truncated USDZ layer"))
    }
    fn expect(&mut self, expected: &str) -> Result<(), MediaError> {
        if self.required()? != expected {
            return Err(invalid("unsupported or missing USDZ profile field"));
        }
        Ok(())
    }
    fn peek(&self, expected: &str) -> Result<bool, MediaError> {
        let mut copy = *self;
        Ok(copy.next()? == Some(expected))
    }
    fn number(&mut self) -> Result<f32, MediaError> {
        let token = self.required()?;
        let value = token.parse::<f32>().map_err(|_| invalid("USDZ requires finite float32 values"))?;
        if !value.is_finite() || value == 0.0 && token.split(['e', 'E']).next().is_some_and(|m| m.bytes().any(|b| matches!(b, b'1'..=b'9'))) {
            return Err(invalid("USDZ number is outside finite float32 range"));
        }
        Ok(value)
    }
    fn integer(&mut self) -> Result<u32, MediaError> {
        let t = self.required()?;
        if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid("USDZ requires nonnegative decimal indices"));
        }
        t.parse().map_err(|_| invalid("USDZ index is outside uint32 range"))
    }
    fn triple(&mut self) -> Result<[f32; 3], MediaError> {
        self.expect("(")?;
        let x = self.number()?;
        self.expect(",")?;
        let y = self.number()?;
        self.expect(",")?;
        let z = self.number()?;
        self.expect(")")?;
        Ok([x, y, z])
    }
    fn vertex_interpolation(&mut self) -> Result<(), MediaError> {
        for t in ["(", "interpolation", "=", "\"vertex\"", ")"] {
            self.expect(t)?;
        }
        Ok(())
    }
}
fn identifier(s: &str) -> bool {
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).is_some_and(|s| {
        !s.is_empty() && s.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}
pub(super) fn unit(n: [f32; 3]) -> Result<[f32; 3], MediaError> {
    let values = n.map(f64::from);
    let len = values.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !len.is_finite() || len == 0.0 {
        return Err(invalid("USDZ normals must be finite and nonzero"));
    }
    Ok(values.map(|v| (v / len) as f32))
}
fn channel(value: f32) -> Result<f32, MediaError> {
    if !(0.0..=1.0).contains(&value) {
        return Err(invalid("USDZ linear RGBA channels must be within 0 through 1"));
    }
    Ok(value)
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Counts {
    pub vertices: usize,
    pub triangles: usize,
}
fn line_breaks(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut count = 0usize;
    for (at, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' || byte == b'\r' && bytes.get(at.saturating_add(1)) != Some(&b'\n') {
            count += 1;
        }
    }
    count
}
pub(super) fn parse(text: &str, limits: &MeshLimits, mut target: Option<&mut Mesh>) -> Result<Counts, MediaError> {
    let rest = text
        .strip_prefix("#usda 1.0\n")
        .or_else(|| text.strip_prefix("#usda 1.0\r\n"))
        .or_else(|| text.strip_prefix("#usda 1.0\r"))
        .ok_or_else(|| invalid("USDZ profile requires a USDA 1.0 text layer"))?;
    let mut lex = Lexer { text: rest, at: 0 };
    lex.expect("(")?;
    let mut metadata = 0u8;
    let mut root = "";
    while !lex.peek(")")? {
        let key = lex.required()?;
        lex.expect("=")?;
        let bit = match key {
            "defaultPrim" => {
                root = lex.required()?;
                if !identifier(root) {
                    return Err(invalid("USDZ defaultPrim must be a plain identifier"));
                }
                1
            }
            "upAxis" => {
                lex.expect("\"Y\"")?;
                2
            }
            "metersPerUnit" => {
                // USDA stage units are double metadata, not a float32 vertex channel.
                if lex.required()?.parse::<f64>().ok() != Some(1.0) {
                    return Err(invalid("USDZ profile requires unscaled metersPerUnit = 1"));
                }
                4
            }
            _ => return Err(invalid("unsupported USDZ layer metadata or composition")),
        };
        if metadata & bit != 0 {
            return Err(invalid("duplicate USDZ layer metadata"));
        }
        metadata |= bit;
    }
    lex.expect(")")?;
    if metadata != 7 || lex.at > 16 * 1024 || rest.get(..lex.at).is_none_or(|s| line_breaks(s) > 64) {
        return Err(invalid("USDZ requires bounded defaultPrim, Y-up and unit metadata"));
    }
    lex.expect("def")?;
    lex.expect("Mesh")?;
    if lex.required()? != root {
        return Err(invalid("USDZ Mesh must match defaultPrim"));
    }
    lex.expect("{")?;
    let mut fields = 0u16;
    let mut counts = [0usize; 6];
    let mut largest_index = None::<u32>;
    let mut bounds = None::<Bounds>;
    while !lex.peek("}")? {
        let kind = lex.required()?;
        let (field, bit) = if kind == "uniform" {
            lex.expect("token")?;
            let name = lex.required()?;
            lex.expect("=")?;
            match name {
                "subdivisionScheme" => {
                    lex.expect("\"none\"")?;
                    (6, 64)
                }
                "orientation" => {
                    lex.expect("\"rightHanded\"")?;
                    (7, 128)
                }
                _ => return Err(invalid("unsupported USDZ uniform field")),
            }
        } else {
            lex.expect("[")?;
            lex.expect("]")?;
            let name = lex.required()?;
            let field = match (kind, name) {
                ("point3f", "points") => 0,
                ("int", "faceVertexCounts") => 1,
                ("int", "faceVertexIndices") => 2,
                ("normal3f", "normals") => 3,
                ("color3f", "primvars:displayColor") => 4,
                ("float", "primvars:displayOpacity") => 5,
                _ => return Err(invalid("unsupported USDZ schema, property or resource")),
            };
            (field, 1u16 << field)
        };
        if fields & bit != 0 {
            return Err(invalid("duplicate USDZ mesh field"));
        }
        fields |= bit;
        if field >= 6 {
            continue;
        }
        lex.expect("=")?;
        lex.expect("[")?;
        let cap = if field == 1 {
            limits.max_triangles
        } else if field == 2 {
            limits.max_triangles.checked_mul(3).ok_or_else(|| invalid("USDZ triangle count overflow"))?
        } else {
            limits.max_vertices
        };
        let mut n = 0usize;
        while !lex.peek("]")? {
            if n >= cap {
                return Err(MediaError::LimitExceeded("USDZ geometry count exceeds its limit".into()));
            }
            match field {
                0 | 3 | 4 => {
                    let mut value = lex.triple()?;
                    if field == 3 {
                        value = unit(value)?;
                    } else if field == 4 {
                        for v in value {
                            channel(v)?;
                        }
                    }
                    if field == 0 {
                        match &mut bounds {
                            Some(b) => {
                                for ((min, max), v) in b.min.iter_mut().zip(&mut b.max).zip(value) {
                                    *min = min.min(v);
                                    *max = max.max(v);
                                }
                            }
                            None => bounds = Some(Bounds { min: value, max: value }),
                        }
                    }
                    if let Some(mesh) = target.as_deref_mut() {
                        match field {
                            0 => *mesh.positions.get_mut(n).ok_or_else(|| invalid("USDZ position extent changed"))? = value,
                            3 => *mesh.normals.get_mut(n).ok_or_else(|| invalid("USDZ normal extent changed"))? = value,
                            _ => {
                                let c = mesh.colors.get_mut(n).ok_or_else(|| invalid("USDZ colour extent changed"))?;
                                for (c, v) in c.iter_mut().take(3).zip(value) {
                                    *c = v;
                                }
                            }
                        }
                    }
                }
                1 | 2 => {
                    let v = lex.integer()?;
                    if field == 1 && v != 3 {
                        return Err(invalid("USDZ profile supports triangle faces only"));
                    }
                    if field == 2 {
                        largest_index = Some(largest_index.map_or(v, |old| old.max(v)));
                        if let Some(mesh) = target.as_deref_mut() {
                            *mesh.indices.get_mut(n).ok_or_else(|| invalid("USDZ index extent changed"))? = v;
                        }
                    }
                }
                _ => {
                    let v = channel(lex.number()?)?;
                    if let Some(mesh) = target.as_deref_mut() {
                        let c = mesh.colors.get_mut(n).ok_or_else(|| invalid("USDZ opacity extent changed"))?;
                        *c.get_mut(3).ok_or_else(|| invalid("USDZ opacity extent changed"))? = v;
                    }
                }
            }
            n = n.checked_add(1).ok_or_else(|| invalid("USDZ count overflow"))?;
            if lex.peek("]")? {
                break;
            }
            lex.expect(",")?;
        }
        lex.expect("]")?;
        if matches!(field, 3..=5) {
            lex.vertex_interpolation()?;
        }
        *counts.get_mut(field).ok_or_else(|| invalid("USDZ field extent changed"))? = n;
    }
    lex.expect("}")?;
    if lex.next()?.is_some() {
        return Err(invalid("USDZ layer has trailing declarations or data"));
    }
    let [vertices, triangles, indices, normals, colors, alpha] = counts;
    if fields != FIELDS
        || vertices == 0
        || triangles == 0
        || indices != triangles.checked_mul(3).ok_or_else(|| invalid("USDZ index count overflow"))?
        || normals != vertices
        || colors != vertices
        || alpha != vertices
        || largest_index.is_none_or(|i| usize::try_from(i).map_or(true, |i| i >= vertices))
    {
        return Err(invalid("USDZ required fields, vertex attributes, triangles or indices disagree"));
    }
    if bounds.and_then(|b| b.checked_sphere()).is_none() {
        return Err(invalid("USDZ geometry has an unrepresentable preview bound"));
    }
    Ok(Counts { vertices, triangles })
}
pub(super) fn text(bytes: &[u8]) -> Result<&str, MediaError> {
    if !bytes.is_ascii() || bytes.iter().any(|b| !matches!(b, b'\t' | b'\r' | b'\n' | 32..=126)) {
        return Err(invalid("USDZ USDA profile requires printable ASCII"));
    }
    if bytes.split(|b| *b == b'\n' || *b == b'\r').any(|line| line.len() > MAX_LINE) {
        return Err(invalid("USDZ line exceeds 1024 bytes"));
    }
    std::str::from_utf8(bytes).map_err(|_| invalid("USDZ USDA profile requires ASCII"))
}
pub(super) fn validate_geometry(mesh: &Mesh) -> Result<(), MediaError> {
    mesh.validate()?;
    if mesh.vertex_count() == 0 || mesh.triangle_count() == 0 {
        return Err(invalid("an empty mesh cannot be written as USDZ"));
    }
    if mesh.bounds().and_then(|b| b.checked_sphere()).is_none() {
        return Err(invalid("USDZ geometry has an unrepresentable preview bound"));
    }
    for triangle in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = triangle.map(|i| usize::try_from(i).ok().and_then(|i| mesh.positions.get(i)).copied());
        let (Some(a), Some(b), Some(c)) = (a, b, c) else {
            return Err(invalid("USDZ triangle index is invalid"));
        };
        let [ax, ay, az] = a.map(f64::from);
        let [bx, by, bz] = b.map(f64::from);
        let [cx, cy, cz] = c.map(f64::from);
        let u = [bx - ax, by - ay, bz - az];
        let v = [cx - ax, cy - ay, cz - az];
        let [x, y, z] = u;
        let [r, s, t] = v;
        let area = [y * t - z * s, z * r - x * t, x * s - y * r].iter().map(|v| v * v).sum::<f64>();
        if !area.is_finite() || area == 0.0 {
            return Err(invalid("USDZ triangle has zero or unrepresentable area"));
        }
    }
    Ok(())
}
pub(super) fn render(mesh: &Mesh, out: &mut dyn Write) -> Result<(), MediaError> {
    let fail = |_: fmt::Error| MediaError::LimitExceeded("USDZ formatted output exceeds its byte limit".into());
    out.write_str("#usda 1.0\n(\n    defaultPrim = \"Mesh\"\n    upAxis = \"Y\"\n    metersPerUnit = 1\n)\ndef Mesh \"Mesh\"\n{\n    uniform token subdivisionScheme = \"none\"\n    uniform token orientation = \"rightHanded\"\n    point3f[] points = [\n").map_err(fail)?;
    for [x, y, z] in &mesh.positions {
        writeln!(out, "        ({x}, {y}, {z}),").map_err(fail)?;
    }
    out.write_str("    ]\n    int[] faceVertexCounts = [\n").map_err(fail)?;
    for _ in mesh.indices.as_chunks::<3>().0 {
        out.write_str("        3,\n").map_err(fail)?;
    }
    out.write_str("    ]\n    int[] faceVertexIndices = [\n").map_err(fail)?;
    for [a, b, c] in mesh.indices.as_chunks::<3>().0 {
        writeln!(out, "        {a}, {b}, {c},").map_err(fail)?;
    }
    out.write_str("    ]\n    normal3f[] normals = [\n").map_err(fail)?;
    for n in &mesh.normals {
        let [x, y, z] = unit(*n)?;
        writeln!(out, "        ({x}, {y}, {z}),").map_err(fail)?;
    }
    out.write_str("    ] (interpolation = \"vertex\")\n    color3f[] primvars:displayColor = [\n").map_err(fail)?;
    for color in &mesh.colors {
        let [r, g, b, a] = *color;
        for v in [r, g, b, a] {
            channel(v)?;
        }
        writeln!(out, "        ({r}, {g}, {b}),").map_err(fail)?;
    }
    out.write_str("    ] (interpolation = \"vertex\")\n    float[] primvars:displayOpacity = [\n").map_err(fail)?;
    for [_, _, _, a] in &mesh.colors {
        writeln!(out, "        {a},").map_err(fail)?;
    }
    out.write_str("    ] (interpolation = \"vertex\")\n}\n").map_err(fail)
}
