//! Provider checks dispatch the same free keys.test command as the API keys dialog.
use super::*;
use nodecraft_engine::{
    commands::Services,
    secrets::{KeyResults, KeyWorker, SecretStore, register_key_worker},
};
use nodecraft_providers::Http;
use std::{
    collections::BTreeSet,
    process::ExitCode,
    time::{Duration, Instant},
};
fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn check(session: &mut Session, results: &KeyResults, account: &str) -> Result<Value, Error> {
    let pending = session.dispatch("keys.test", &json!({"account":account}))?;
    let id = pending["pending"].as_u64().ok_or_else(|| invalid("key check returned no pending id"))?;
    let start = Instant::now();
    loop {
        for outcome in results.drain() {
            if outcome.id == id {
                return outcome.result;
            }
        }
        if start.elapsed() > Duration::from_millis(nodecraft_engine::key_test::TEST_TIMEOUT_MS.saturating_add(6000)) {
            return Err(Error::new(ErrorKind::Timeout, "provider check did not finish in time"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn run(
    selected: Option<&str>,
    mut maker: impl FnMut(&str) -> Result<Arc<dyn Provider>, Readiness>,
    store: Arc<dyn SecretStore>,
    http: Arc<dyn Http>,
) -> Result<Value, Error> {
    let mut ids = crate::random_bytes();
    let mut session = nodecraft_automation::blank_session(nodecraft_types::ProjectId::from_bytes(ids()), nodecraft_types::CanvasId::from_bytes(ids()))?;
    let catalog = session.generation_catalog()?.run_catalog();
    let adapters: BTreeSet<String> = catalog.catalog().models().map(|m| m.provider.clone()).collect();
    if selected.is_some_and(|name| !adapters.contains(name)) {
        return Err(invalid("unknown provider id; use providers list"));
    }
    let mut services = Services::default();
    let runner = crate::run::Runner::new(&mut services, "cli-provider-checks".into())?;
    let (worker, results) = KeyWorker::spawn_with_tests(store, Some(runner.key_test_host(http)?))?;
    register_key_worker(&mut services, worker)?;
    session = session.with_services(services);
    let mut verified: BTreeMap<String, Value> = BTreeMap::new();
    let mut rows = Vec::new();
    for adapter in adapters.iter().filter(|name| selected.is_none_or(|selected| name.as_str() == selected)) {
        let outcome = match maker(adapter) {
            Err(readiness) => json!({"state":"not_ready","reason":readiness.message(adapter)}),
            Ok(_) => match provider_setup::key_account(adapter) {
                Some(account) => {
                    if let Some(previous) = verified.get(account) {
                        previous.clone()
                    } else {
                        let outcome = match check(&mut session, &results, account) {
                            Ok(result) => json!({"state":result["result"],"reason":result["reason"]}),
                            Err(error) => json!({"state":"failed","reason":error.message}),
                        };
                        verified.insert(account.to_owned(), outcome.clone());
                        outcome
                    }
                }
                None => json!({"state":"unverifiable","reason":"this adapter has no provider-key check; use comfy servers --test to check a ComfyUI server"}),
            },
        };
        let mut row = outcome;
        row["id"] = json!(adapter);
        rows.push(row);
    }
    Ok(Value::Array(rows))
}
pub(super) fn command(arguments: &[String]) -> ExitCode {
    let result = (|| {
        let options = crate::options(arguments, &["--json"]).map_err(|s| Error::new(ErrorKind::InvalidInput, s))?;
        let selected = match options.positional.as_slice() {
            [] => None,
            [name] if name == "all" => None,
            [name] => Some(name.as_str()),
            _ => return Err(invalid("providers test takes one provider id or all")),
        };
        let maker = keychain_maker(nodecraft_engine::settings::peek().providers)?;
        let http = Arc::new(nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?);
        let report = run(selected, maker, Arc::new(KeyringStore), http)?;
        let failed = report.as_array().into_iter().flatten().any(|r| matches!(r["state"].as_str(), Some("failed" | "not_ready")));
        let printed = crate::print(options.json, &report, |report| {
            report
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| {
                    format!(
                        "{}: {}{}\n",
                        r["id"].as_str().unwrap_or_default(),
                        r["state"].as_str().unwrap_or_default(),
                        r["reason"].as_str().map(|s| format!(" — {s}")).unwrap_or_default()
                    )
                })
                .collect()
        });
        Ok::<_, Error>(if failed { ExitCode::FAILURE } else { printed })
    })();
    match result {
        Ok(code) => code,
        Err(error) => crate::fail(&error.message),
    }
}
#[cfg(test)]
mod tests;
