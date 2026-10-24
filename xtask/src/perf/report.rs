use serde_json::{Value, json};
use std::{collections::BTreeSet, io::Read, path::Path};
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const SCHEMA: &str = "nodecraft.bench/2";
pub const WORKLOAD: &str = "m1-152-v1";
pub const BUDGET_POLICY: &str = "baseline-quantile-plus-3iqr-v1";
const MAX_NS: u64 = 3_600_000_000_000;
fn err() -> String {
    "invalid or incomplete benchmark report".into()
}
pub fn read(path: &Path) -> Result<Value, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "cannot read benchmark report")?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read benchmark report")?;
    parse(&bytes)
}
pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_BYTES {
        return Err("benchmark report exceeds 8 MiB".into());
    }
    // Reject excessive shape/string widths BEFORE allocating serde's owned Value tree.
    let (mut depth, mut tokens, mut width) = (0usize, 0usize, 0usize);
    let (mut string, mut escape) = (false, false);
    for b in bytes {
        if string {
            width = width.saturating_add(1);
            if width > 4096 {
                return Err(err());
            }
            if escape {
                escape = false;
            } else if *b == b'\\' {
                escape = true;
            } else if *b == b'"' {
                string = false;
            }
        } else {
            match b {
                b'"' => {
                    string = true;
                    width = 0;
                    tokens += 1;
                }
                b'{' | b'[' => {
                    depth += 1;
                    tokens += 1;
                }
                b'}' | b']' => {
                    depth = depth.checked_sub(1).ok_or_else(err)?;
                }
                b',' | b':' => tokens += 1,
                _ => {}
            }
        }
        if depth > 24 || tokens > 131_072 {
            return Err(err());
        }
    }
    serde_json::from_slice(bytes).map_err(|_| err())
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(Value::as_str).filter(|s| s.len() <= 4096).ok_or_else(err)
}
fn number(v: &Value, k: &str) -> Result<u64, String> {
    v.get(k).and_then(Value::as_u64).ok_or_else(err)
}
fn keys(v: &Value, expected: &[&str]) -> Result<(), String> {
    let actual: BTreeSet<_> = v.as_object().ok_or_else(err)?.keys().map(String::as_str).collect();
    if actual != expected.iter().copied().collect() {
        return Err(err());
    }
    Ok(())
}
fn oid(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn expected_ids() -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for shape in ["chain", "fan_in", "fan_out", "disconnected"] {
        for n in [200, 1000] {
            for op in ["apply", "undo", "redo"] {
                ids.insert(format!("graph/{shape}/{n}/{op}"));
            }
            ids.insert(format!("engine/{shape}/{n}/dispatch"));
            for kind in ["native_storage_wall", "memory_storage_wall"] {
                for op in ["save", "load"] {
                    ids.insert(format!("{kind}/{shape}/{n}/{op}"));
                }
            }
        }
    }
    for n in [200, 1000] {
        for ppp in [1, 2] {
            for op in ["pan", "zoom"] {
                ids.insert(format!("canvas/{n}/{ppp}/{op}"));
            }
        }
    }
    ids
}
pub fn samples(v: &Value) -> Result<Vec<u64>, String> {
    let raw = v.get("samples_ns").and_then(Value::as_array).filter(|a| (3..=256).contains(&a.len())).ok_or_else(err)?;
    raw.iter().map(|v| v.as_u64().filter(|n| *n <= MAX_NS).ok_or_else(err)).collect()
}
pub fn percentile(raw: &[u64], p: usize) -> Option<u64> {
    let mut v = raw.to_vec();
    v.sort_unstable();
    let rank = p.saturating_mul(v.len()).saturating_add(99) / 100;
    v.get(rank.saturating_sub(1)).copied()
}
fn allowance(raw: &[u64]) -> u64 {
    percentile(raw, 75).unwrap_or(0).saturating_sub(percentile(raw, 25).unwrap_or(0)).saturating_mul(3)
}
pub fn budgets(base: &Value) -> Result<Value, String> {
    let rows = base.get("records").and_then(Value::as_array).ok_or_else(err)?;
    let mut values = vec![];
    for row in rows {
        let raw = samples(row)?;
        let band = allowance(&raw);
        let mut quantiles = serde_json::Map::new();
        for q in [50, 95, 99] {
            quantiles.insert(format!("p{q}"), json!(percentile(&raw, q).ok_or_else(err)?.saturating_add(band)));
        }
        values.push(json!({"id":text(row,"id")?,"baseline_stats_ns":stats(&raw),"allowance_ns":band,"budgets_ns":quantiles}));
    }
    Ok(json!({"policy":BUDGET_POLICY,"informational":true,"stability_validated":false,"workloads":values}))
}
pub fn stats(raw: &[u64]) -> Value {
    let p50 = percentile(raw, 50);
    let deviations: Vec<_> = raw.iter().map(|n| n.abs_diff(p50.unwrap_or(0))).collect();
    json!({"samples":raw.len(),"min":raw.iter().min(),"max":raw.iter().max(),"p25":percentile(raw,25),"p50":p50,"p75":percentile(raw,75),"iqr":percentile(raw,75).unwrap_or(0).saturating_sub(percentile(raw,25).unwrap_or(0)),"p95":percentile(raw,95),"p99":percentile(raw,99),"mad":percentile(&deviations,50),"tail_qualification":if raw.len()<100{"insufficient_for_p99_comparison"}else{"descriptive_nearest_rank; not a confidence interval"}})
}
fn validate_perf3(v: &Value, n: u64, frames: usize) -> Result<(), String> {
    // Headless records have no presentation source (#526): presentation stays unsupported.
    if text(v, "schema")? != "nodecraft.perf/3" || v.get("clock") != Some(&json!(true)) || v.get("presentation_source") != Some(&Value::Null) {
        return Err(err());
    }
    let metrics = v.get("metrics").and_then(Value::as_array).filter(|a| a.len() == 10).ok_or_else(err)?;
    let names: BTreeSet<_> = metrics.iter().filter_map(|m| m.get("metric").and_then(Value::as_str)).collect();
    if names
        != ["frame_prepare", "frame_interval", "dispatch", "graph_stage", "graph_stage_refused", "presentation", "undo", "redo", "load", "save"]
            .into_iter()
            .collect()
    {
        return Err(err());
    }
    for metric in metrics {
        let name = text(metric, "metric")?;
        let count = number(metric, "samples")?;
        if text(metric, "unit")? != "us"
            || count > 4096
            || number(metric, "recorded")? < count
            || metric.get("supported") != Some(&json!(name != "presentation"))
        {
            return Err(err());
        }
        let mut previous = 0;
        for q in ["p50", "p95", "p99", "max"] {
            if count == 0 {
                if metric.get(q) != Some(&Value::Null) {
                    return Err(err());
                }
            } else {
                let value = number(metric, q)?;
                if value < previous || value > MAX_NS / 1000 {
                    return Err(err());
                }
                previous = value;
            }
        }
    }
    if v.pointer("/environment/profile").and_then(Value::as_str) != Some("release") {
        return Err(err());
    }
    let find = |id: &str| metrics.iter().find(|m| m.get("metric").and_then(Value::as_str) == Some(id)).ok_or_else(err);
    let prepare = find("frame_prepare")?;
    if prepare.get("supported") != Some(&json!(true))
        || number(prepare, "samples")? < u64::try_from(frames).map_err(|_| err())?
        || number(prepare, "invalid")? != 0
    {
        return Err(err());
    }
    for q in ["p50", "p95", "p99"] {
        number(prepare, q)?;
    }
    let presentation = find("presentation")?;
    if presentation.get("supported") != Some(&json!(false))
        || number(presentation, "samples")? != 0
        || text(presentation, "unavailable_reason")? != "presentation_timestamps_not_available"
    {
        return Err(err());
    }
    if number(v.get("workload").ok_or_else(err)?, "total_nodes")? != n {
        return Err(err());
    }
    Ok(())
}
pub fn validate(v: &Value) -> Result<(), String> {
    keys(v, &["schema", "workload_version", "definitions", "source", "environment", "binaries", "records", "unsupported"])?;
    if text(v, "schema")? != SCHEMA || text(v, "workload_version")? != WORKLOAD || !oid(text(v, "definitions")?) {
        return Err(err());
    }
    let source = v.get("source").ok_or_else(err)?;
    keys(source, &["revision", "fingerprint", "dirty"])?;
    if !oid(text(source, "revision")?) || !oid(text(source, "fingerprint")?) || source.get("dirty").and_then(Value::as_bool).is_none() {
        return Err(err());
    }
    let env = v.get("environment").ok_or_else(err)?;
    keys(env, &["machine", "cpu_model", "flags_fingerprint", "cargo", "rustc", "target", "os", "arch", "logical_cpus", "profile"])?;
    for k in ["machine", "cpu_model", "flags_fingerprint", "cargo", "rustc", "target", "os", "arch"] {
        if text(env, k)?.is_empty() {
            return Err(err());
        }
    }
    if !oid(text(env, "flags_fingerprint")?) {
        return Err(err());
    }
    let machine = text(env, "machine")?;
    if machine.len() > 64
        || !machine.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || text(env, "profile")? != "release"
        || !(1..=4096).contains(&number(env, "logical_cpus")?)
    {
        return Err(err());
    }
    let binaries = v.get("binaries").ok_or_else(err)?;
    keys(binaries, &["graph", "engine", "canvas"])?;
    for k in ["graph", "engine", "canvas"] {
        if !oid(text(binaries, k)?) {
            return Err(err());
        }
    }
    if v.get("unsupported") != Some(&json!(["gpu_timing", "presentation_timing", "wasm_runtime"])) {
        return Err(err());
    }
    let records = v.get("records").and_then(Value::as_array).filter(|v| v.len() <= 128).ok_or_else(err)?;
    let mut found = BTreeSet::new();
    for record in records {
        keys(record, &["id", "measurement", "shape", "nodes", "edges", "operation", "warmups", "samples_ns", "context", "perf2"])?;
        let id = text(record, "id")?;
        if !found.insert(id.to_owned()) {
            return Err(err());
        }
        let n = number(record, "nodes")?;
        if ![200, 1000].contains(&n) || !(1..=16).contains(&number(record, "warmups")?) {
            return Err(err());
        }
        let shape = text(record, "shape")?;
        let op = text(record, "operation")?;
        let kind = text(record, "measurement")?;
        let raw = samples(record)?;
        let expected = match kind {
            "graph_cpu" => format!("graph/{shape}/{n}/{op}"),
            "dispatch_cpu" => format!("engine/{shape}/{n}/{op}"),
            "native_storage_wall" | "memory_storage_wall" => format!("{kind}/{shape}/{n}/{op}"),
            "canvas_cpu" => id.into(),
            _ => return Err(err()),
        };
        if expected != id {
            return Err(err());
        }
        let context = record.get("context").filter(|v| v.is_object()).ok_or_else(err)?;
        if kind == "canvas_cpu" {
            if shape != "grid" || !["pan", "zoom"].contains(&op) {
                return Err(err());
            }
            let ppp = number(context, "ppp")?;
            if ![1, 2].contains(&ppp)
                || id != format!("canvas/{n}/{ppp}/{op}")
                || context.get("viewport") != Some(&json!([1600, 1000]))
                || context.get("capture") != Some(&json!(true))
            {
                return Err(err());
            }
            let frames = if op == "pan" { 120 } else { 160 };
            if raw.len() != frames {
                return Err(err());
            }
            let loads = context.get("workloads").and_then(Value::as_array).filter(|v| v.len() == frames).ok_or_else(err)?;
            for load in loads {
                if number(load, "total_nodes")? != n
                    || number(load, "total_edges")? != number(record, "edges")?
                    || number(load, "visible_edges")? > number(load, "total_edges")?
                    || number(load, "visible_nodes")? > n
                    || load.get("visibility_unavailable") != Some(&Value::Null)
                {
                    return Err(err());
                }
                let visible = number(load, "visible_nodes")?;
                if op == "pan" && ((n == 200 && visible != n) || (n == 1000 && visible >= 500)) {
                    return Err(err());
                }
            }
            // The key predates perf/3; renaming it would orphan existing records.
            validate_perf3(record.get("perf2").ok_or_else(err)?, n, frames)?;
        } else {
            if !["chain", "fan_in", "fan_out", "disconnected"].contains(&shape)
                || number(record, "edges")? != if shape == "disconnected" { 0 } else { n - 1 }
                || record.get("perf2") != Some(&Value::Null)
            {
                return Err(err());
            }
            if kind == "graph_cpu" && !["apply", "undo", "redo"].contains(&op)
                || kind == "dispatch_cpu" && op != "dispatch"
                || kind.ends_with("storage_wall") && !["save", "load"].contains(&op)
            {
                return Err(err());
            }
        }
    }
    if found != expected_ids() {
        return Err("incomplete workload matrix".into());
    }
    Ok(())
}
fn markdown_text(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '`' => escaped.push_str("&#96;"),
            '\\' => escaped.push_str("&#92;"),
            '|' => escaped.push_str("&#124;"),
            ':' => escaped.push_str("&#58;"),
            '/' => escaped.push_str("&#47;"),
            '(' => escaped.push_str("&#40;"),
            ')' => escaped.push_str("&#41;"),
            '[' => escaped.push_str("&#91;"),
            ']' => escaped.push_str("&#93;"),
            '*' => escaped.push_str("&#42;"),
            '_' => escaped.push_str("&#95;"),
            '#' => escaped.push_str("&#35;"),
            '~' => escaped.push_str("&#126;"),
            // Line and paragraph separators are not control scalars but still break a summary line.
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => escaped.push(' '),
            c => escaped.push(c),
        }
    }
    escaped
}
fn comparison_header(base: &Value, current: &Value) -> Result<String, String> {
    // Only allowlisted, already-public envelope fields; never configuration or local tool paths.
    // Validated text is <=4096 bytes; escaping expands each character by at most six bytes.
    let mut header = String::from("# Performance comparison\n\n");
    for (label, report) in [("Baseline", base), ("Current", current)] {
        let source = report.get("source").ok_or_else(err)?;
        header.push_str(&format!(
            "{label}: revision {}; source fingerprint {}; definitions {}; dirty {}.\n\n",
            text(source, "revision")?,
            text(source, "fingerprint")?,
            text(report, "definitions")?,
            source.get("dirty").and_then(Value::as_bool).ok_or_else(err)?,
        ));
    }
    let environment = base.get("environment").ok_or_else(err)?;
    // `machine` is already allowlisted by validate; escaping it anyway keeps this call safe if that rule widens.
    header.push_str(&format!(
        "Shared machine class: {}; CPU model: {}.\n\n",
        markdown_text(text(environment, "machine")?),
        markdown_text(text(environment, "cpu_model")?)
    ));
    if [base, current].iter().any(|r| r.pointer("/source/dirty").and_then(Value::as_bool) == Some(true)) {
        header.push_str("**Dirty source warning:** at least one measurement includes uncommitted source; its revision alone does not identify the measured tree. Use the recorded source fingerprint.\n\n");
    }
    Ok(header)
}
pub fn compare(base: &Value, current: &Value) -> Result<(bool, String), String> {
    validate(base)?;
    validate(current)?;
    for field in ["workload_version", "definitions", "environment", "unsupported"] {
        if base.get(field) != current.get(field) {
            return Err(format!("incomparable {field}; establish a deliberate new baseline"));
        }
    }
    if base.pointer("/environment/cpu_model").and_then(Value::as_str) == Some("unavailable") {
        return Err("incomparable: CPU model unavailable".into());
    }
    let a = base.get("records").and_then(Value::as_array).ok_or_else(err)?;
    let b = current.get("records").and_then(Value::as_array).ok_or_else(err)?;
    let mut markdown = comparison_header(base, current)?;
    markdown.push_str(
        "Informational, matched nearest-rank quantiles. Policy baseline-quantile-plus-3iqr-v1: each matched baseline quantile plus three times that workload's interquartile range. This provisional engineering allowance is not a confidence interval, stable SLA or FPS target. Isolated extremes remain in range/max, without inflating every budget. A new calibration is required after incompatible workload/environment changes.\n\n| Workload | Quantile | Baseline ns | Current ns | Change ns | Change % | Budget ns | Signal |\n|---|---|---:|---:|---:|---:|---:|---|\n",
    );
    let mut regression = false;
    let mut dispersion = String::from(
        "\n## Ordered sample distributions\n\nLarge IQR/baseline ratios still mean poor sensitivity; no stable hardware budget is established. Range/max retain isolated interruptions.\n\n",
    );
    for old in a {
        let id = text(old, "id")?;
        let new = b.iter().find(|v| v.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(err)?;
        for k in ["measurement", "shape", "nodes", "edges", "operation", "warmups", "context"] {
            if old.get(k) != new.get(k) {
                return Err(format!("incomparable workload definition/counts for {id}"));
            }
        }
        let x = samples(old)?;
        let y = samples(new)?;
        if x.len() < 100 || y.len() < 100 {
            return Err(format!("incomplete tails for {id}: at least 100 ordered samples required for p99 comparison"));
        }
        let band = allowance(&x);
        for q in [50, 95, 99] {
            let u = percentile(&x, q).ok_or_else(err)?;
            let v = percentile(&y, q).ok_or_else(err)?;
            let delta = i128::from(v) - i128::from(u);
            let budget = u.saturating_add(band);
            let signal = v > budget;
            regression |= signal;
            let percent = if u == 0 { "unavailable: zero baseline".into() } else { format!("{:.2}", 100. * (v as f64 - u as f64) / u as f64) };
            markdown.push_str(&format!(
                "| {id} | p{q} | {u} | {v} | {delta} | {percent} | {budget} | {} |\n",
                if signal { "regression signal" } else { "within observed envelope" }
            ));
        }
        dispersion.push_str(&format!("- `{id}`: baseline `{}`, current `{}`; allowance `{band} ns`.\n\n", stats(&x), stats(&y)));
    }
    markdown.push_str(&dispersion);
    markdown.push_str(if regression {
        "\nVerdict: **regression signal**; investigate on stable hardware. Baseline was not rewritten.\n"
    } else {
        "\nVerdict: **within observed envelopes for all measured quantiles**; not proof of a stable performance target.\n"
    });
    Ok((regression, markdown))
}
