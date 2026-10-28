//! CSV rows use independent documents, frozen inputs and the same run/consent path as the canvas.
use crate::{
    inputs::{Binding, Files, Target},
    run::{Consent, Runner, Spend},
};
use nodecraft_engine::commands::Services;
use nodecraft_types::{Currency, Error, ErrorKind, Money, NodeId};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
struct Args {
    options: crate::Options,
    csv: PathBuf,
    quote_only: bool,
    keep_going: bool,
    max_cost: BTreeMap<String, u64>,
    max_total: Option<(String, u64)>,
    timeout: Duration,
}
fn args(args: &[String]) -> Result<Args, Error> {
    let mut common = Vec::new();
    let mut csv = None;
    let mut quote_only = false;
    let mut keep_going = false;
    let mut max_cost = BTreeMap::new();
    let mut max_total = None;
    let mut timeout = Duration::from_secs(3600);
    let mut timeout_set = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || rest.next().cloned().ok_or_else(|| invalid(format!("{arg} needs a value")));
        match arg.as_str() {
            "--csv" if csv.is_some() => return Err(invalid("--csv is given twice")),
            "--csv" => csv = Some(PathBuf::from(value()?)),
            "--quote" => quote_only = true,
            "--keep-going" => keep_going = true,
            "--max-cost" => {
                let (code, amount) = crate::serve::parse_max_cost(&value()?).map_err(invalid)?;
                if max_cost.insert(code, amount).is_some() {
                    return Err(invalid("a currency limit is given twice"));
                }
            }
            "--max-total" if max_total.is_some() => return Err(invalid("--max-total is given twice")),
            "--max-total" => max_total = Some(crate::serve::parse_max_total(&value()?).map_err(invalid)?),
            "--run-timeout" => {
                if timeout_set {
                    return Err(invalid("--run-timeout is given twice"));
                }
                let seconds = value()?.parse::<u64>().map_err(|_| invalid("--run-timeout takes whole seconds"))?;
                if !(1..=604800).contains(&seconds) {
                    return Err(invalid("--run-timeout takes 1 to 604800 seconds"));
                }
                timeout = Duration::from_secs(seconds);
                timeout_set = true;
            }
            "--input" | "--technique" | "--canvas" | "--node" | "--out" => {
                common.push(arg.clone());
                common.push(value()?);
            }
            _ => common.push(arg.clone()),
        }
    }
    let mut options = crate::options(&common, &["--json", "--input", "--technique", "--canvas", "--node", "--out", "--yes", "--estimate"]).map_err(invalid)?;
    options.native_assets = !quote_only;
    if options.positional.len() != 1 {
        return Err(invalid("batch needs one project or technique target"));
    }
    if !quote_only && options.out.is_none() {
        return Err(invalid("batch needs --out <existing folder>, or --quote"));
    }
    Ok(Args { options, csv: csv.ok_or_else(|| invalid("batch needs --csv <file>"))?, quote_only, keep_going, max_cost, max_total, timeout })
}
fn read_csv(path: &Path) -> Result<String, Error> {
    if !path.is_file() {
        return Err(invalid("--csv must name a regular file"));
    }
    let file = std::fs::File::open(path).map_err(|_| invalid("CSV file cannot be opened"))?;
    let mut text = String::new();
    file.take((crate::csv::MAX_BYTES + 1) as u64).read_to_string(&mut text).map_err(|_| invalid("CSV must be readable UTF-8"))?;
    if text.len() > crate::csv::MAX_BYTES {
        return Err(invalid("CSV exceeds 8 MiB"));
    }
    Ok(text)
}
fn rows(records: Vec<Vec<String>>, defaults: &[String]) -> Result<Vec<Vec<String>>, Error> {
    rows_bounded(records, defaults, 128 * 1024 * 1024)
}
fn rows_bounded(records: Vec<Vec<String>>, defaults: &[String], max_bytes: usize) -> Result<Vec<Vec<String>>, Error> {
    let mut iter = records.into_iter();
    let headers = iter.next().ok_or_else(|| invalid("CSV needs a header and data rows"))?;
    let mut names = BTreeSet::new();
    for header in &headers {
        if header.trim().is_empty() || header.len() > 256 || header.contains('=') || !names.insert(header.clone()) {
            return Err(invalid("CSV headers must be unique nonempty input names"));
        }
    }
    let mut base = Vec::new();
    let mut default_names = BTreeSet::new();
    for default in defaults {
        let (key, value) = default.split_once('=').ok_or_else(|| invalid("--input needs name=value"))?;
        if !default_names.insert(key) {
            return Err(invalid("a default input is given twice"));
        }
        base.push((key, value));
    }
    let mut out = Vec::new();
    let mut remaining = max_bytes;
    for (index, row) in iter.enumerate() {
        if row.len() != headers.len() {
            return Err(invalid(format!("CSV data row {} has a different number of fields", index.saturating_add(1))));
        }
        // Explicit command-line defaults come first; CSV columns preserve controller ordering.
        let defaults = base.iter().filter(|(key, _)| !names.contains(*key));
        if defaults.clone().count().saturating_add(headers.len()) > crate::inputs::MAX_BINDINGS {
            return Err(invalid("a row has more than 256 inputs"));
        }
        let mut values = Vec::new();
        for (key, value) in defaults.copied().chain(headers.iter().zip(row.iter()).map(|(k, v)| (k.as_str(), v.as_str()))) {
            let bytes = key.len().checked_add(value.len()).and_then(|n| n.checked_add(1)).ok_or_else(|| invalid("batch assignment extent overflow"))?;
            remaining = remaining.checked_sub(bytes).ok_or_else(|| invalid("expanded batch assignments exceed 128 MiB"))?;
            // Admission precedes allocation, including repeated command-line defaults.
            values.push(format!("{key}={value}"));
        }
        out.push(values);
    }
    if out.is_empty() {
        return Err(invalid("CSV has no data rows"));
    }
    Ok(out)
}
fn add_totals(total: &mut BTreeMap<String, u64>, quote: &Value) -> Result<(), Error> {
    for (code, amount) in quote["totals"].as_object().into_iter().flatten() {
        let micros = amount["micros"].as_u64().ok_or_else(|| invalid("invalid quote amount"))?;
        let entry = total.entry(code.clone()).or_default();
        *entry = entry.checked_add(micros).ok_or_else(|| Error::new(ErrorKind::Overflow, "batch price total overflow"))?;
    }
    Ok(())
}
fn limits(quote: &Value, maximum: &BTreeMap<String, u64>) -> Result<(), Error> {
    if maximum.is_empty() {
        return Ok(());
    }
    if quote["complete"] != true {
        return Err(Error::new(ErrorKind::Budget, "a price limit cannot cover an unknown-priced row"));
    }
    for (code, amount) in quote["totals"].as_object().into_iter().flatten() {
        let micros = amount["micros"].as_u64().ok_or_else(|| invalid("invalid quote amount"))?;
        if micros > maximum.get(code).copied().unwrap_or(0) {
            return Err(Error::new(ErrorKind::Budget, "a row exceeds its --max-cost limit"));
        }
    }
    Ok(())
}
fn total_limit(totals: &BTreeMap<String, u64>, complete: bool, maximum: &Option<(String, u64)>) -> Result<(), Error> {
    if let Some((currency, maximum)) = maximum
        && (!complete || totals.iter().any(|(code, amount)| *amount > 0 && (code != currency || amount > maximum)))
    {
        return Err(Error::new(ErrorKind::Budget, "batch exceeds --max-total or includes an unknown/different-currency price"));
    }
    Ok(())
}
fn targets(session: &nodecraft_engine::Session, target: &Target) -> Result<Vec<NodeId>, Error> {
    if !target.nodes.is_empty() {
        return crate::quote::targets(session, target.canvas, &target.nodes);
    }
    // A row must still return cached outputs when its inputs match the saved project. The
    // engine decides reuse; filtering to stale nodes here would silently produce an empty row.
    let graph = session.document().canvases().get(&target.canvas).ok_or_else(|| invalid("batch canvas is missing"))?;
    Ok(graph.nodes().keys().copied().collect())
}
fn session(target: &Target, bindings: &[Binding], services: Services) -> Result<nodecraft_engine::Session, Error> {
    let mut session = target.session(services)?;
    for binding in bindings {
        binding.apply(&mut session, target.canvas)?;
    }
    Ok(session)
}
fn row_folder(out: &Path, count: usize) -> Result<Vec<PathBuf>, Error> {
    let mut ids = crate::random_bytes();
    for _ in 0..64 {
        let name = NodeId::from_bytes(ids()).to_string();
        let dir = out.join(format!("batch-{name}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                let mut rows = Vec::new();
                for i in 0..count {
                    let path = dir.join(format!("row-{:06}", i.saturating_add(1)));
                    std::fs::create_dir(&path).map_err(|_| invalid("cannot create all row output folders; no row has run"))?;
                    rows.push(path);
                }
                return Ok(rows);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(invalid("cannot create the batch output folder; no row has run")),
        }
    }
    Err(Error::new(ErrorKind::Conflict, "cannot allocate a unique batch output folder"))
}
fn execute(args: &Args, target: &Target, bindings: &[Vec<Binding>], maker: &mut crate::quote::Maker<'_>) -> Result<Value, Error> {
    let out = args.options.out.as_deref().map(Path::new).map(crate::run::out_folder).transpose()?;
    let spend = Spend::default();
    if let Some((code, micros)) = &args.max_total {
        let currency = Currency::try_from(code.clone()).map_err(|_| invalid("invalid total currency"))?;
        spend.budgets.lock().map_err(|_| invalid("batch budget lock unavailable"))?.project = Some(Money { currency, micros: *micros });
    }
    let mut runner = Runner::with_spend(&mut Services::default(), "cli-batch".into(), &spend)?;
    let mut quotes = Vec::new();
    let mut totals = BTreeMap::new();
    let mut all_complete = true;
    let mut refusal = None;
    for (index, row) in bindings.iter().enumerate() {
        let mut session = session(target, row, runner.services()?)?;
        let wanted = targets(&session, target)?;
        crate::quote::prepare(&mut session, target.canvas, &wanted, maker)?;
        runner.install(&mut session);
        if args.options.estimate {
            crate::estimates::for_targets(&mut runner, &mut session, target.canvas, &wanted)?;
        }
        let quote = crate::quote::quote(&mut session, target.canvas, &wanted)?;
        add_totals(&mut totals, &quote)?;
        all_complete &= quote["complete"] == true;
        limits(&quote, &args.max_cost)?;
        if !args.quote_only
            && let Err(error) = crate::run::check_consent(&quote, Consent { spend: args.options.yes })
        {
            refusal.get_or_insert(error);
        }
        quotes.push(json!({"row":index.saturating_add(1),"allowed":quote["allowed"],"complete":quote["complete"],"totals":quote["totals"],"steps":quote["steps_total"]}));
    }
    total_limit(&totals, all_complete, &args.max_total)?;
    if args.quote_only {
        return Ok(json!({"quoted":true,"complete":all_complete,"totals_micros":totals,"rows":quotes}));
    }
    if let Some(error) = refusal {
        return Err(error);
    }
    let folders = out.as_deref().map(|out| row_folder(out, bindings.len())).transpose()?.unwrap_or_default();
    let mut reports = Vec::new();
    let mut stopped = false;
    for (index, row) in bindings.iter().enumerate() {
        if stopped {
            reports.push(json!({"row":index.saturating_add(1),"status":"skipped"}));
            continue;
        }
        let result = (|| {
            let mut session = session(target, row, runner.services()?)?;
            let wanted = targets(&session, target)?;
            crate::quote::prepare(&mut session, target.canvas, &wanted, maker)?;
            runner.install(&mut session);
            if args.options.estimate {
                crate::estimates::for_targets(&mut runner, &mut session, target.canvas, &wanted)?;
            }
            let quote = crate::quote::quote(&mut session, target.canvas, &wanted)?;
            limits(&quote, &args.max_cost)?;
            let deadline = Instant::now().checked_add(args.timeout).ok_or_else(|| invalid("run timeout overflow"))?;
            let steps = runner.run_within(&mut session, target.canvas, &wanted, &quote, Consent { spend: args.options.yes }, Some(deadline))?;
            let kept = steps
                .iter()
                .filter(|step| matches!(step["status"].as_str(), Some("recorded" | "unaccounted" | "reused")))
                .filter_map(|step| step["node"].as_str()?.parse().ok())
                .collect::<Vec<_>>();
            let results = folders.get(index).map(|out| crate::run::export(&mut session, target.canvas, &kept, out)).transpose()?.unwrap_or_default();
            let failed =
                steps.iter().any(|step| !matches!(step["status"].as_str(), Some("recorded" | "reused"))) || results.iter().any(|r| r["status"] == "failed");
            Ok::<_, Error>(json!({"row":index.saturating_add(1),"status":if failed{"failed"}else{"done"},"quote":quote,"steps":steps,"results":results}))
        })();
        let report = match result {
            Ok(report) => report,
            Err(error) => json!({"row":index.saturating_add(1),"status":"failed","reason":error.message}),
        };
        if report["status"] == "failed" && !args.keep_going {
            stopped = true;
        }
        reports.push(report);
    }
    Ok(json!({"quoted":false,"totals_micros":totals,"rows":reports}))
}
pub fn command(arguments: &[String]) -> ExitCode {
    let result = (|| {
        let args = args(arguments)?;
        let records = crate::csv::parse(&read_csv(&args.csv)?)?;
        let rows = rows(records, &args.options.inputs)?;
        let source = args.options.positional.first().ok_or_else(|| invalid("missing batch target"))?;
        let target = Target::open(source, &args.options)?;
        let csv = args.csv.canonicalize().map_err(|_| invalid("cannot resolve CSV path"))?;
        let base = csv.parent().ok_or_else(|| invalid("CSV has no parent folder"))?;
        let mut files = Files::default();
        let mut prepared = Vec::new();
        let mut text = 0usize;
        for (index, row) in rows.iter().enumerate() {
            let bindings =
                target.bindings(row, base, &mut files).map_err(|e| Error::new(e.kind, format!("CSV data row {}: {}", index.saturating_add(1), e.message)))?;
            text = bindings.iter().map(Binding::text_bytes).fold(text, usize::saturating_add);
            if text > 128 * 1024 * 1024 {
                return Err(invalid("staged batch text exceeds 128 MiB"));
            }
            prepared.push(bindings);
        }
        drop(rows);
        let mut maker = crate::providers::keychain_maker(nodecraft_engine::settings::peek().providers)?;
        let report = execute(&args, &target, &prepared, &mut maker)?;
        let failed = report["rows"].as_array().into_iter().flatten().any(|r| matches!(r["status"].as_str(), Some("failed" | "skipped")));
        let printed = crate::print(args.options.json, &report, text_report);
        Ok::<_, Error>(if failed { ExitCode::FAILURE } else { printed })
    })();
    match result {
        Ok(code) => code,
        Err(error) => crate::fail(&error.message),
    }
}
fn text_report(report: &Value) -> String {
    let mut text = if report["quoted"] == true { format!("Batch quote (micros by currency): {}\n", report["totals_micros"]) } else { String::new() };
    for row in report["rows"].as_array().into_iter().flatten() {
        text.push_str(&format!("Row {}: {}\n", row["row"], row["status"].as_str().unwrap_or("quoted")));
        if let Some(reason) = row["reason"].as_str() {
            text.push_str(reason);
            text.push('\n');
        }
        if row["steps"].is_array() {
            text.push_str(&crate::run::report_text(row));
        }
    }
    text
}
#[cfg(test)]
mod tests;
