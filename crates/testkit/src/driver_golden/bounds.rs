//! Borrowed admission, before materializing converted/record/report representations.
use nodecraft_exec::run::StepResult;
use nodecraft_nodes::GenerateOutput;
use nodecraft_providers::{Completed, Output};
use nodecraft_types::Value;
pub type Check<T> = Result<T, &'static str>;
pub const MIB: usize = 1024 * 1024;
#[derive(Default, Clone, Copy, Debug)]
pub struct Charge {
    pub raw: usize,
    pub items: usize,
    pub owned: usize,
    pub escaped: usize,
}
fn add(a: usize, b: usize) -> Check<usize> {
    a.checked_add(b).ok_or("ownership_overflow")
}
fn mul(a: usize, b: usize) -> Check<usize> {
    a.checked_mul(b).ok_or("ownership_overflow")
}
impl Charge {
    pub fn plus(self, b: Self) -> Check<Self> {
        Ok(Self { raw: add(self.raw, b.raw)?, items: add(self.items, b.items)?, owned: add(self.owned, b.owned)?, escaped: add(self.escaped, b.escaped)? })
    }
    fn string(n: usize) -> Check<Self> {
        Ok(Self { raw: n, items: 1, owned: add(32, n)?, escaped: add(2, mul(6, n)?)? })
    }
}
pub fn value(v: &Value, depth: usize) -> Check<Charge> {
    if depth > 30 {
        return Err("value_depth");
    }
    let base = Charge { raw: 256, items: 16, owned: 256, escaped: 512 };
    let c = match v {
        Value::Text(s) => {
            if s.len() > 65536 {
                return Err("fixture_text");
            }
            Charge::string(s.len())?
        }
        Value::Media(a) => Charge::string(a.mime.essence().len())?,
        Value::List(v) => {
            let mut c = Charge::default();
            for x in v {
                c = c.plus(value(x, depth + 1)?)?;
                if c.items > 16384 {
                    return Err("value_items");
                }
            }
            c
        }
        _ => Charge::default(),
    };
    base.plus(c)
}
pub fn completion(c: &Completed, routes: &[GenerateOutput]) -> Check<(usize, usize)> {
    if c.outputs.len() > 16 || routes.len() > 16 {
        return Err("completion_shape");
    }
    let mut raw = 0;
    let mut converted = 1024;
    for o in &c.outputs {
        match o {
            Output::Text(s) => {
                raw = add(raw, s.len())?;
                converted = add(converted, add(256, mul(6, s.len())?)?)?;
            }
            Output::Bytes { mime, bytes } => {
                if mime.len() > 64 {
                    return Err("completion_mime");
                }
                raw = add(raw, bytes.len())?;
                // Bytes can become owned UTF-8 text, regardless of claimed MIME/BOM.
                // Reserve text-sized ownership even when real classification later refuses it.
                converted = add(converted, add(add(2048, mime.len())?, mul(6, bytes.len())?)?)?;
            }
            Output::Url { .. } => return Err("completion_url"),
        }
    }
    let mut longest_port = 0;
    for route in routes {
        longest_port = longest_port.max(route.port.len());
        converted = add(converted, add(256, mul(6, route.port.len())?)?)?;
    }
    // The chosen port is copied for every output occurrence, including aliased Bytes.
    converted = add(converted, mul(c.outputs.len(), add(256, mul(6, longest_port)?)?)?)?;
    if raw > 8192 {
        return Err("completion_payload");
    }
    Ok((raw, converted))
}
pub fn routed(outputs: &[(String, Value)]) -> Check<usize> {
    let mut n = 0;
    for (p, v) in outputs {
        n = add(n, add(add(128, p.len())?, value(v, 0)?.owned)?)?;
    }
    Ok(n)
}
pub fn record(results: &[StepResult]) -> Check<Charge> {
    let mut c = Charge { raw: 64, items: 4, owned: 256, escaped: 512 };
    if results.len() > 64 {
        return Err("record_count");
    }
    for r in results {
        c = c.plus(Charge { raw: 512, items: 32, owned: 1024, escaped: 1024 })?;
        for v in &r.generation.outputs {
            c = c.plus(value(v, 0)?)?;
        }
    }
    if c.raw > MIB || c.items > 16384 {
        return Err("record_envelope");
    }
    if c.owned > 16 * MIB {
        return Err("record_ownership");
    }
    Ok(c)
}
pub fn json(v: &serde_json::Value, depth: usize) -> Check<Charge> {
    if depth > 64 {
        return Err("json_depth");
    }
    let mut c = Charge { raw: 0, items: 1, owned: 256, escaped: 128 };
    match v {
        serde_json::Value::String(s) => c = c.plus(Charge::string(s.len())?)?,
        serde_json::Value::Array(a) => {
            for v in a {
                c = c.plus(json(v, depth + 1)?)?;
            }
        }
        serde_json::Value::Object(o) => {
            for (k, v) in o {
                c = c.plus(Charge::string(k.len())?)?.plus(json(v, depth + 1)?)?;
            }
        }
        _ => {}
    }
    if c.items > 16384 {
        return Err("json_items");
    }
    Ok(c)
}
/// Independent cumulative ledgers; moving is free but reservations live until outcome drops.
#[derive(Default, Debug)]
pub struct Ownership {
    pub original: usize,
    pub delivery_metadata: usize,
    pub conversion: usize,
    pub run: usize,
    pub admin: usize,
    pub callbacks: usize,
    pub serialized: usize,
    pub epochs: usize,
    pub attempts: usize,
    pub commands: usize,
}
impl Ownership {
    pub fn charge(slot: &mut usize, n: usize, cap: usize) -> Check<()> {
        let next = add(*slot, n)?;
        if next > cap {
            return Err("ownership_limit");
        }
        *slot = next;
        Ok(())
    }
    pub fn epoch(&mut self, attempts: usize) -> Check<usize> {
        if attempts == 0 || attempts > 8 || self.epochs >= 128 {
            return Err("epoch_limit");
        }
        let next = add(self.attempts, attempts)?;
        if next > 1024 {
            return Err("attempt_limit");
        }
        let epoch = self.epochs;
        self.epochs += 1;
        self.attempts = next;
        Ok(epoch)
    }
    pub fn command(&mut self) -> Check<()> {
        Self::charge(&mut self.commands, 1, 256)
    }
}

pub fn params(p: &serde_json::Map<String, serde_json::Value>) -> Check<Charge> {
    let mut c = Charge { owned: 256, items: 1, ..Default::default() };
    for (k, v) in p {
        c = c.plus(Charge::string(k.len())?)?.plus(json(v, 1)?)?;
    }
    Ok(c)
}

/// Reserve both queued request copies and retry/captured request owners before provider_job.
pub fn work(w: &nodecraft_nodes::JobSpec, key: &str, attempts: usize) -> Check<usize> {
    let nodecraft_nodes::JobSpec::Generate { provider, model, params, inputs, outputs, .. } = w else { return Ok(0) };
    let mut n = add(2048, add(provider.len(), add(model.len(), key.len())?)?)?;
    for (k, v) in params {
        n = add(n, add(mul(6, k.len())?, mul(6, value(v, 0)?.owned)?)?)?;
    }
    for i in inputs {
        n = add(n, add(i.role.len(), mul(6, value(&i.value, 0)?.owned)?)?)?;
    }
    for o in outputs {
        n = add(n, add(256, o.port.len())?)?;
    }
    mul(n, mul(8, add(1, attempts)?)?)
}

/// Capped borrowed serialization counts fragments/escaping without an owned body.
struct BorrowCounter {
    n: usize,
    cap: usize,
    fragment: usize,
}
impl std::io::Write for BorrowCounter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = b
            .len()
            .checked_mul(6)
            .and_then(|n| n.checked_add(self.fragment))
            .and_then(|n| self.n.checked_add(n))
            .filter(|n| *n <= self.cap)
            .ok_or_else(|| std::io::Error::other("fixture metadata limit"))?;
        self.n = n;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn canvas(canvas: &nodecraft_graph::Canvas, cap: usize) -> Check<usize> {
    let mut counter = BorrowCounter { n: 0, cap, fragment: 256 };
    serde_json::to_writer(&mut counter, canvas).map_err(|_| "fixture_canvas_ownership")?;
    Ok(counter.n)
}
pub fn catalog(catalog: &nodecraft_catalog::Catalog, cap: usize) -> Check<usize> {
    use std::io::Write;
    if catalog.len() > 64 {
        return Err("catalog_models");
    }
    // Selected retains one schema clone per unique model. Model field strings and schema
    // structure have multiple quoted/delimited fragments;128 per fragment overcharges their
    // actual headers, while6*bytes covers copied strings/escaping. Full catalog is metered.
    let mut counter = BorrowCounter { n: 0, cap, fragment: 128 };
    for model in catalog.models() {
        counter.write_all(&[0; 128]).map_err(|_| "catalog_ownership")?;
        for text in [&model.id, &model.provider, &model.provider_model, &model.label, &model.description] {
            counter.write_all(text.as_bytes()).map_err(|_| "catalog_ownership")?;
        }
        for mode in &model.modes {
            counter.write_all(&[0; 32]).map_err(|_| "catalog_ownership")?;
            counter.write_all(mode.id.as_bytes()).map_err(|_| "catalog_ownership")?;
            for role in &mode.inputs {
                counter.write_all(&[0; 32]).map_err(|_| "catalog_ownership")?;
                counter.write_all(role.role.as_bytes()).map_err(|_| "catalog_ownership")?;
            }
        }
        if let nodecraft_catalog::Pricing::PerUnit { count: Some(count), .. } = &model.pricing {
            counter.write_all(count.as_bytes()).map_err(|_| "catalog_ownership")?;
        }
        serde_json::to_writer(&mut counter, &model.params).map_err(|_| "catalog_ownership")?;
    }
    Ok(counter.n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cumulative_epochs_do_not_reset_when_job_ids_are_reused() {
        let mut b = Ownership::default();
        for i in 0..128 {
            assert_eq!(b.epoch(8).unwrap(), i);
        }
        assert_eq!(b.attempts, 1024);
        assert_eq!(b.epoch(1), Err("epoch_limit"));
        assert_eq!(b.epochs, 128);
        assert_eq!(b.attempts, 1024);
    }
    #[test]
    fn record_total_rejects_many_individually_bounded_results_before_json() {
        let results: Vec<_> = (0..64)
            .map(|n| StepResult {
                node: nodecraft_types::NodeId::from_bytes([n; 16]),
                generation: nodecraft_graph::Generation::new(
                    nodecraft_types::GenerationId::from_bytes([n; 16]),
                    vec![Value::Text("x".repeat(8192)), Value::Text("y".repeat(8192))],
                ),
            })
            .collect();
        assert!(record(&results).is_err());
        assert_eq!(results.len(), 64);
        assert!(matches!(&results[0].generation.outputs[0],Value::Text(s)if s.len()==8192));
    }
    #[test]
    fn borrowed_value_walk_bounds_depth_and_items() {
        let mut v = Value::Text("public".into());
        for _ in 0..31 {
            v = Value::List(vec![v]);
        }
        assert!(value(&v, 0).is_err());
        assert!(value(&Value::List(vec![Value::Bool(true); 1025]), 0).is_err());
    }
}

#[cfg(test)]
mod correction_tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn byte_aliases_and_repeated_route_names_cover_actual_converted_owners() {
        let body: Arc<[u8]> = Arc::from(&b"public utf8 body"[..]);
        let done = Completed::new((0..16).map(|_| Output::Bytes { mime: "text/plain".into(), bytes: body.clone() }).collect());
        let routes = vec![GenerateOutput { port: "p".repeat(2048), modality: nodecraft_types::Modality::Text, style_family: None }];
        let (_, charge) = completion(&done, &routes).unwrap();
        let produced = nodecraft_engine::generate::produced(&routes, &done).unwrap();
        let actual: usize = produced
            .outputs
            .iter()
            .map(|(port, v)| {
                port.capacity()
                    + match v {
                        Value::Text(text) => text.capacity(),
                        _ => 0,
                    }
            })
            .sum();
        assert!(charge >= actual);
        assert_eq!(produced.outputs.len(), 16);
        let invalid = Completed::new(vec![Output::Bytes { mime: "application/octet-stream".into(), bytes: Arc::from(vec![0; 8192]) }]);
        assert!(completion(&invalid, &routes).unwrap().1 >= 6 * 8192);
    }
    #[test]
    fn conversion_arithmetic_overflow_is_refusal() {
        assert!(add(usize::MAX, 1).is_err());
        assert!(mul(usize::MAX, 6).is_err());
    }
}
