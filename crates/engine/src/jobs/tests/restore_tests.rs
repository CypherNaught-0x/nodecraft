use super::*;

#[test]
fn invalid_provider_restore_is_observable() {
    let (runner, budget) = budgeted(None);
    let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
    runner.register(JobId(1), priced(provider.clone(), Some(10)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    let log = budget.log.lock().unwrap().clone();
    // This exact public call compiles before and after #365. The previous unit result hides refusal.
    let observed = format!(
        "{:?}",
        runner.restore_generation("x".repeat(513), "saved".into(), 3, JobHandle { remote_id: "accepted".into(), data: serde_json::Value::Null })
    );
    assert_eq!(*budget.log.lock().unwrap(), log);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
    assert!(observed.starts_with("Err("), "refusal must be observable, got {observed}");
}

fn saved() -> JobHandle {
    JobHandle { remote_id: "remote-secret".into(), data: serde_json::json!({"token": "payload-secret"}) }
}

#[test]
fn refused_handle_is_recovered_without_copying_and_diagnostics_are_public_safe() {
    let (runner, _) = budgeted(None);
    for provider in ["provider-secret".repeat(40), "é".repeat(257)] {
        let original = saved();
        let remote = original.remote_id.as_ptr();
        let payload = original.data.get("token").unwrap().as_str().unwrap().as_ptr();
        let refused = runner.restore_handle(provider, "key-secret".into(), original).unwrap_err();
        assert_eq!(refused.error().kind, nodecraft_types::ErrorKind::InvalidInput);
        let diagnostics = format!("{refused} {refused:?} {:?}", std::error::Error::source(&refused));
        for secret in ["provider-secret", "key-secret", "remote-secret", "payload-secret", "é"] {
            assert!(!diagnostics.contains(secret), "diagnostic leaked input");
        }
        let recovered = refused.into_handle();
        assert_eq!(recovered.remote_id.as_ptr(), remote);
        assert_eq!(recovered.data.get("token").unwrap().as_str().unwrap().as_ptr(), payload);
        assert_eq!(recovered.remote_id, "remote-secret");
        assert_eq!(recovered.data, serde_json::json!({"token": "payload-secret"}));
        // Correct only the host's address metadata. The same accepted handle resumes, no submit.
        let resumes = Arc::new(ResumesWith(usd(7)));
        runner.register(JobId(1), priced(resumes, Some(10)));
        runner.restore_handle("resumes", "nodecraft:r:1".into(), recovered).unwrap();
        assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    }
}

struct LegacyAdapter {
    name: String,
    polled: Mutex<Vec<String>>,
    submits: AtomicU64,
}
impl Provider for LegacyAdapter {
    fn id(&self) -> &str {
        &self.name
    }
    fn submit<'a>(&'a self, _: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        self.submits.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(ProviderError::new(ErrorKind::Server, "unexpected submit")) })
    }
    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        self.polled.lock().unwrap().push(handle.remote_id.clone());
        Box::pin(async { Ok(JobProgress::Done(Completed::new(vec![]))) })
    }
    fn cancel<'a>(&'a self, _: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn valid_default_addresses_and_legacy_keys_keep_exact_generation_resume_semantics() {
    let named = nodecraft_types::ProviderTarget::named("local", nodecraft_types::ProviderInstanceId::from_bytes([0; 16])).unwrap();
    for name in [String::new(), "é:/arbitrary".into(), "x".repeat(512), named.lane_name().unwrap()] {
        for base in [String::new(), "base".into(), "é".repeat(100), "x".repeat(100)] {
            for generation in [0, 3] {
                let (mut runner, _) = budgeted(None);
                runner.budget = None;
                let provider = Arc::new(LegacyAdapter { name: name.clone(), polled: Mutex::default(), submits: AtomicU64::new(0) });
                let target = nodecraft_types::ProviderTarget::default_instance(&name).unwrap();
                assert_ne!(target, named);
                runner.restore_generation(name.clone(), base.clone(), generation, saved()).unwrap();
                assert!(runner.lock().handles.contains_key(&(target.clone(), sent_key(&base, generation))));
                assert_eq!(runner.lock().failures.get(&(target, base.clone())).copied().unwrap_or(0), generation);
                runner.register(JobId(1), priced(provider.clone(), None));
                let outcome = block_on(runner.run(Start { idempotency_key: base.clone(), ..job(1) }, CancelToken::default()));
                if base.is_empty() || base.len() > MAX_SENT_KEY || !base.is_ascii() {
                    // Restoration compatibility does not broaden the existing Start contract.
                    assert!(matches!(outcome, Outcome::Failed { transient: false, .. }));
                    assert!(provider.polled.lock().unwrap().is_empty());
                } else {
                    assert_eq!(outcome, Outcome::Done);
                    assert_eq!(*provider.polled.lock().unwrap(), ["remote-secret"]);
                }
                assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[test]
fn refusal_preserves_live_replacement_lease_handles_and_all_accounting() {
    let (runner, budget) = budgeted(None);
    let provider = Arc::new(Flaky { submits: AtomicU64::new(0), polls: AtomicU64::new(0), fail_polls: 100 });
    runner.register(JobId(1), priced(provider.clone(), Some(10)));
    assert!(matches!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Failed { transient: true, .. }));
    runner.register(JobId(1), priced(provider.clone(), Some(20)));
    let log = budget.log.lock().unwrap().clone();
    let (registration, lease, handles, failures, reserved, metadata, deferred, invocations) = {
        let s = runner.lock();
        (
            Arc::clone(&s.jobs[&JobId(1)].identity),
            s.jobs[&JobId(1)].scheduler.clone().unwrap(),
            s.handles.clone(),
            s.failures.clone(),
            format!("{:?}", s.reserved),
            s.metadata.keys().cloned().collect::<Vec<_>>(),
            (s.settle_after.clone(), s.forget_after.clone()),
            s.invocations.clone(),
        )
    };
    let held = budget.log.lock().unwrap().metadata_statistics().held;
    let error = runner.restore_generation("x".repeat(513), "nodecraft:r:1".into(), u32::MAX, saved()).unwrap_err();
    {
        let s = runner.lock();
        assert!(Arc::ptr_eq(&registration, &s.jobs[&JobId(1)].identity));
        let after = s.jobs[&JobId(1)].scheduler.as_ref().unwrap();
        assert!(Arc::ptr_eq(&lease.identity, &after.identity));
        assert_eq!(lease.invocation, after.invocation);
        assert_eq!(s.handles, handles);
        assert_eq!(s.failures, failures);
        assert_eq!(format!("{:?}", s.reserved), reserved);
        assert_eq!(s.metadata.keys().cloned().collect::<Vec<_>>(), metadata);
        assert_eq!((s.settle_after.clone(), s.forget_after.clone()), deferred);
        assert_eq!(s.invocations, invocations);
    }
    assert_eq!(*budget.log.lock().unwrap(), log);
    assert_eq!(budget.log.lock().unwrap().metadata_statistics().held, held);
    assert_eq!(error.into_handle().remote_id, "remote-secret");
    // Existing replacement still resumes the original accepted request, never a fresh submit.
    provider.polls.store(100, Ordering::SeqCst);
    assert_eq!(block_on(runner.run(job(1), CancelToken::default())), Outcome::Done);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 1);
}
