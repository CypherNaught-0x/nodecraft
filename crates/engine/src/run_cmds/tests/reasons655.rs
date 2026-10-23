use super::*;
use nodecraft_exec::planner::{Blocker, Reason};
use std::sync::atomic::{AtomicUsize, Ordering};

struct PrivateSource {
    descriptor: NodeDescriptor,
    calls: Arc<AtomicUsize>,
}
impl NodeKind for PrivateSource {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("source").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::new(ErrorKind::InvalidInput, "private-native-hook-canary"))
    }
}

#[test]
fn replacing_a_builtin_id_does_not_publish_a_native_hooks_message() {
    let original = kinds();
    assert!(original.has_public_errors("source.text"));
    let calls = Arc::new(AtomicUsize::new(0));
    let source = PrivateSource { descriptor: original.get("source.text").unwrap().descriptor().clone(), calls: Arc::clone(&calls) };
    let replacement = original.with_replaced("source.text", vec![Box::new(source)]).unwrap();
    assert!(!replacement.has_public_errors("source.text"));
    assert!(replacement.has_public_errors("source.image"));
    assert!(original.clone().has_public_errors("source.text"));
    let mut resources = Resources::default();
    resources.insert(RunCatalog::new(Catalog::from_toml(CATALOG).unwrap()).unwrap());
    resources.insert(BudgetState { budgets: Default::default(), spent: Default::default() });
    let starts = Arc::new(AtomicUsize::new(0));
    let sink = Arc::clone(&starts);
    let mut services = Services::default();
    services
        .register_run_host(move |_| {
            sink.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"accepted": true}))
        })
        .unwrap();
    let mut session =
        Session::with_registries(project(), Registry::builtins().unwrap(), replacement).unwrap().with_resources(resources).with_services(services);
    let quote = session.dispatch("run.quote", &request(&[1])).unwrap();
    assert!(calls.load(Ordering::SeqCst) > 0, "the native callback must actually have failed");
    assert_eq!(quote["allowed"], false);
    let output = quote.to_string();
    assert!(output.contains("node_validation"));
    assert!(!output.contains("private-native-hook"));
    let refused = session
        .dispatch("run.start", &json!({"canvas": C, "mode": "selected", "nodes": [id(1)], "accept": {"token": quote["token"], "amounts": {}}}))
        .unwrap_err();
    assert!(refused.message.contains("node_validation"), "{refused:?}");
    assert!(!refused.message.contains("private-native-hook"));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
}

#[test]
fn public_validation_text_is_unicode_safe_and_shares_the_report_cap() {
    let blockers: Vec<_> = (0..101).map(|_| Blocker { node: id(2), port: None, upstream: None, reason: Reason::PublicInvalid("雪".repeat(5000)) }).collect();
    let output = blocked_reasons(&blockers);
    assert!(output[0].contains(&"雪".repeat(341)), "a 1024-byte limit ends at a Unicode boundary");
    assert!(output.join("; ").len() <= BLOCKED_BYTES);
    assert!(output.last().unwrap().contains("blockers_omitted"));
    assert!(output.len() <= 100);
}
