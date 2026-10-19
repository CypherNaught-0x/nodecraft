use super::*;
use futures::executor::block_on;
fn job(key: &str) -> ProviderJob {
    ProviderJob { model: "m".into(), params: Default::default(), inputs: vec![], idempotency_key: key.into() }
}
fn failures(count: u32) -> Option<MockFailures> {
    Some(MockFailures { count, kind: ErrorKind::Network })
}
fn operation(id: &str, keys: &[&str], script: MockScript) -> MockOperation {
    MockOperation { id: id.into(), model: "m".into(), sent_keys: keys.iter().map(|k| (*k).into()).collect(), script }
}
fn provider(polls: u32, script: MockScript, keys: &[&str]) -> MockProvider {
    MockProvider::scripted("mock", [("m".into(), MockModel { polls, ..MockModel::new(MockOutput::Text) })], [operation("op", keys, script)]).unwrap()
}
#[test]
fn finite_submit_refusals_then_success_are_unbilled_until_acceptance() {
    for n in [0, 1, 3] {
        let p = provider(0, MockScript { submit_failures: failures(n), ..Default::default() }, &["k"]);
        for _ in 0..n {
            assert_eq!(block_on(p.submit(&job("k"))).unwrap_err().kind, ErrorKind::Network);
            assert_eq!(p.billed(), 0);
        }
        assert!(matches!(block_on(p.submit(&job("k"))).unwrap(), Submission::Finished(_)));
        assert_eq!(p.billed(), 1);
        assert_eq!(p.requests().len(), n as usize + 1);
    }
}
#[test]
fn finite_lost_replies_accept_once_before_success() {
    for n in [0, 1, 3] {
        let p = provider(0, MockScript { lost_replies: failures(n), ..Default::default() }, &["k"]);
        for _ in 0..n {
            assert!(block_on(p.submit(&job("k"))).is_err());
            assert_eq!(p.billed(), 1);
        }
        assert!(matches!(block_on(p.submit(&job("k"))).unwrap(), Submission::Finished(_)));
        assert_eq!(p.billed(), 1);
    }
}
#[test]
fn finite_poll_transport_errors_pause_one_handles_progress() {
    for n in [0, 1, 3] {
        let p = provider(2, MockScript { poll_failures: failures(n), ..Default::default() }, &["k"]);
        let Submission::Queued(h) = block_on(p.submit(&job("k"))).unwrap() else { panic!("queued") };
        for _ in 0..n {
            assert!(block_on(p.poll(&h)).is_err());
        }
        assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Running { .. }));
        assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Done(_)));
        assert_eq!(p.billed(), 1);
        assert_eq!(p.polls(), u64::from(n) + 2);
    }
}
#[test]
fn finite_terminal_generations_require_distinct_ordered_keys() {
    for n in [0, 1, 3] {
        let keys: Vec<String> = (0..=n).map(|i| format!("k{i}")).collect();
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let p = provider(1, MockScript { failed_generations: Some(MockFailures { count: n, kind: ErrorKind::JobFailed }), ..Default::default() }, &refs);
        for (i, key) in keys.iter().enumerate() {
            let Submission::Queued(h) = block_on(p.submit(&job(key))).unwrap() else { panic!("queued") };
            let done = block_on(p.poll(&h)).unwrap();
            if i < n as usize {
                assert!(matches!(done, JobProgress::Failed(e) if e.kind==ErrorKind::JobFailed));
            } else {
                assert!(matches!(done, JobProgress::Done(_)));
            }
        }
        assert_eq!(p.billed(), n as usize + 1);
    }
}

fn queued(p: &MockProvider, key: &str) -> JobHandle {
    let Submission::Queued(h) = block_on(p.submit(&job(key))).unwrap() else { panic!("queued") };
    h
}
fn model(polls: u32) -> MockModel {
    MockModel { polls, ..MockModel::new(MockOutput::Text) }
}
fn build(models: Vec<(String, MockModel)>, ops: Vec<MockOperation>) -> Result<MockProvider, ProviderError> {
    MockProvider::scripted("mock", models, ops)
}
#[test]
fn exact_generation_order_and_terminal_replay_are_sticky() {
    let p = provider(2, MockScript { failed_generations: Some(MockFailures { count: 1, kind: ErrorKind::Server }), ..Default::default() }, &["a", "b", "c"]);
    assert!(block_on(p.submit(&job("b"))).is_err());
    let h = queued(&p, "a");
    assert!(block_on(p.submit(&job("b"))).is_err());
    assert!(block_on(p.submit(&job("c"))).is_err());
    assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Running { .. }));
    assert!(block_on(p.submit(&job("b"))).is_err());
    for _ in 0..2 {
        assert!(matches!(block_on(p.poll(&h)).unwrap(),JobProgress::Failed(e) if e.kind==ErrorKind::Server));
    }
    assert_eq!(queued(&p, "a"), h);
    let next = queued(&p, "b");
    assert!(matches!(block_on(p.poll(&next)).unwrap(), JobProgress::Running { .. }));
    assert!(matches!(block_on(p.poll(&next)).unwrap(), JobProgress::Done(_)));
    assert!(block_on(p.submit(&job("c"))).is_err());
    assert_eq!(p.billed(), 2);
}
#[test]
fn accepted_identity_precedes_every_changed_payload_and_script() {
    let p = MockProvider::scripted(
        "mock",
        [("m".into(), model(2)), ("other".into(), model(1)), ("plain".into(), MockModel { fail: Some(MockFailure::Submit(ErrorKind::Auth)), ..model(0) })],
        [
            operation("one", &["a"], MockScript::default()),
            MockOperation {
                id: "two".into(),
                model: "other".into(),
                sent_keys: vec!["b".into()],
                script: MockScript { submit_failures: failures(1), ..Default::default() },
            },
        ],
    )
    .unwrap();
    let mut wrong = job("a");
    wrong.model = "plain".into();
    assert!(block_on(p.submit(&wrong)).is_err());
    assert_eq!(p.billed(), 0);
    let h = queued(&p, "a");
    assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Running { .. }));
    for name in ["unknown", "other", "plain", "m"] {
        let mut changed = job("a");
        changed.model = name.into();
        changed.params.insert("secret".into(), serde_json::json!("different"));
        changed.inputs = vec![JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(b"different".as_slice()) }];
        assert!(matches!(block_on(p.submit(&changed)).unwrap(),Submission::Queued(got) if got==h));
    }
    assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Done(_)));
    let mut b = job("b");
    b.model = "other".into();
    assert!(block_on(p.submit(&b)).is_err());
    assert!(block_on(p.submit(&b)).is_ok());
    assert!(block_on(p.submit(&job("undeclared"))).is_err());
    assert_eq!(p.billed(), 2);
}
#[test]
fn operations_and_models_have_independent_countdowns_and_clones_share_them() {
    let script = MockScript { submit_failures: failures(1), ..Default::default() };
    let make = || {
        MockProvider::scripted(
            "mock",
            [("m".into(), model(0)), ("n".into(), model(0))],
            [
                operation("a", &["a"], script),
                operation("b", &["b"], script),
                MockOperation { id: "c".into(), model: "n".into(), sent_keys: vec!["c".into()], script },
            ],
        )
        .unwrap()
    };
    let p = make();
    let clone = p.clone();
    assert!(block_on(p.submit(&job("a"))).is_err());
    assert!(block_on(clone.submit(&job("a"))).is_ok());
    assert!(block_on(clone.submit(&job("b"))).is_err());
    assert!(block_on(p.submit(&job("b"))).is_ok());
    let mut c = job("c");
    c.model = "n".into();
    assert!(block_on(p.submit(&c)).is_err());
    assert!(block_on(clone.submit(&c)).is_ok());
    assert_eq!(p.billed(), 3);
    assert_eq!(clone.billed(), 3);
    let reset = make();
    assert!(block_on(reset.submit(&job("a"))).is_err());
    assert_eq!(reset.billed(), 0);
    assert_eq!(p.billed(), 3);
}
#[test]
fn cancellation_and_terminal_failure_override_remaining_lost_replies() {
    let script =
        MockScript { lost_replies: failures(3), failed_generations: Some(MockFailures { count: 1, kind: ErrorKind::JobFailed }), ..Default::default() };
    for cancel in [false, true] {
        let p = provider(1, script, &["a", "b"]);
        assert!(block_on(p.submit(&job("a"))).is_err());
        assert_eq!(p.billed(), 1);
        // The fixture knows the deterministic accepted handle despite the lost response.
        let h = JobHandle { remote_id: "mock-0".into(), data: serde_json::Value::Null };
        if cancel {
            block_on(p.cancel(&h)).unwrap();
        }
        assert!(matches!(block_on(p.poll(&h)).unwrap(),JobProgress::Failed(e) if e.kind==if cancel {ErrorKind::Cancelled} else {ErrorKind::JobFailed}));
        assert_eq!(queued(&p, "a"), h);
        assert_eq!(queued(&p, "a"), h);
        if cancel {
            assert!(block_on(p.submit(&job("b"))).is_err());
        } else {
            assert!(block_on(p.submit(&job("b"))).is_err());
            assert_eq!(p.billed(), 2);
        }
    }
}
#[test]
fn queued_success_keeps_remaining_lost_reply_slots_and_cancel_is_sticky() {
    let p = provider(1, MockScript { lost_replies: failures(3), ..Default::default() }, &["a", "b"]);
    assert!(block_on(p.submit(&job("a"))).is_err());
    let h = JobHandle { remote_id: "mock-0".into(), data: serde_json::Value::Null };
    assert!(matches!(block_on(p.poll(&h)).unwrap(), JobProgress::Done(_)));
    assert!(block_on(p.submit(&job("a"))).is_err());
    assert!(block_on(p.submit(&job("a"))).is_err());
    assert_eq!(queued(&p, "a"), h);
    block_on(p.cancel(&h)).unwrap();
    assert!(matches!(block_on(p.poll(&h)).unwrap(),JobProgress::Failed(e) if e.kind==ErrorKind::Cancelled));
    assert!(block_on(p.submit(&job("b"))).is_err());
    assert_eq!(p.billed(), 1);
}
#[test]
fn zero_stages_normalize_before_sync_compatibility_and_dropped_futures_have_effects() {
    let z = failures(0);
    let p = provider(0, MockScript { submit_failures: z, lost_replies: z, poll_failures: z, failed_generations: z }, &["a"]);
    let request = job("a");
    drop(p.submit(&request));
    assert_eq!(p.billed(), 1);
    assert_eq!(p.requests().len(), 1);
    let p = provider(0, MockScript { submit_failures: failures(1), ..Default::default() }, &["a"]);
    drop(p.submit(&request));
    assert_eq!(p.billed(), 0);
    assert!(block_on(p.submit(&request)).is_ok());
}
#[test]
fn reconstructed_instances_can_alias_handles_without_sharing_state() {
    let a = provider(1, MockScript::default(), &["a"]);
    let b = provider(1, MockScript::default(), &["b"]);
    let ha = queued(&a, "a");
    let hb = queued(&b, "b");
    assert_eq!(ha, hb);
    block_on(a.cancel(&ha)).unwrap();
    assert!(matches!(block_on(b.poll(&ha)).unwrap(), JobProgress::Done(_)), "cross-instance use aliases, it is not detected");
    assert!(matches!(block_on(a.poll(&ha)).unwrap(),JobProgress::Failed(e) if e.kind==ErrorKind::Cancelled));
}
#[test]
fn permanent_legacy_models_and_delivery_contract_stay_unchanged() {
    for fail in [MockFailure::Submit(ErrorKind::Auth), MockFailure::Poll(ErrorKind::Network), MockFailure::Job(ErrorKind::JobFailed)] {
        let p = MockProvider::scripted("mock", [("m".into(), MockModel { fail: Some(fail), ..model(0) })], []).unwrap();
        for _ in 0..4 {
            assert!(block_on(p.submit(&job("a"))).is_err());
        }
        assert_eq!(p.billed(), 0);
    }
    // Pre-invocation cancellation consumes no script slots; legacy delivery remains unknown.
    struct Stop;
    impl crate::CancelSignal for Stop {
        fn is_cancelled(&self) -> bool {
            true
        }
        fn cancelled(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }
    struct Clock;
    impl crate::Clock for Clock {
        fn now_ms(&self) -> u64 {
            0
        }
        fn sleep(&self, _: u64) -> BoxFuture<'static, ()> {
            Box::pin(async {})
        }
    }
    let p = provider(0, MockScript { submit_failures: failures(1), ..Default::default() }, &["a"]);
    assert!(block_on(crate::run_to_completion(&p, &job("a"), &Stop, &Clock, crate::RunLimits::default(), |_| {})).is_err());
    assert!(p.requests().is_empty());
    assert!(block_on(p.submit(&job("a"))).is_err());
    assert!(block_on(p.submit(&job("a"))).is_ok());
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn concurrent_clones_linearize_countdowns_and_accept_only_once() {
    use std::sync::Barrier;
    let p = provider(0, MockScript { submit_failures: failures(3), ..Default::default() }, &["a"]);
    let barrier = Arc::new(Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let p = p.clone();
            let b = barrier.clone();
            std::thread::spawn(move || {
                b.wait();
                block_on(p.submit(&job("a"))).is_ok()
            })
        })
        .collect();
    let successes = workers.into_iter().map(|worker| usize::from(worker.join().unwrap())).sum::<usize>();
    assert_eq!(successes, 1);
    assert_eq!(p.billed(), 1);
    assert_eq!(p.requests().len(), 4);
    assert!(block_on(p.submit(&job("a"))).is_ok());
    assert_eq!(p.billed(), 1);
}
#[test]
fn constructor_rejects_invalid_ownership_stages_and_counts() {
    let base = || operation("op", &["a"], MockScript::default());
    assert!(build(vec![("m".into(), model(0)), ("m".into(), model(0))], vec![]).is_err());
    for n in [1025, u32::MAX] {
        assert!(build(vec![("m".into(), model(1))], vec![operation("op", &["a"], MockScript { submit_failures: failures(n), ..Default::default() })]).is_err());
    }
    for field in [MockScript { poll_failures: failures(1), ..Default::default() }, MockScript { failed_generations: failures(1), ..Default::default() }] {
        assert!(build(vec![("m".into(), model(0))], vec![operation("op", &["a", "b"], field)]).is_err());
    }
    assert!(build(vec![("m".into(), model(1))], vec![operation("op", &["a"], MockScript { failed_generations: failures(1), ..Default::default() })]).is_err());
    assert!(build(vec![("m".into(), model(0))], vec![base(), base()]).is_err());
    let mut other = base();
    other.id = "other".into();
    assert!(build(vec![("m".into(), model(0))], vec![base(), other]).is_err());
    let mut missing = base();
    missing.model = "missing".into();
    assert!(build(vec![("m".into(), model(0))], vec![missing]).is_err());
    assert!(build(vec![("m".into(), MockModel { fail: Some(MockFailure::Submit(ErrorKind::Network)), ..model(0) })], vec![base()]).is_err());
    for bad in ["".into(), "é".into(), "x".repeat(65)] {
        let mut op = base();
        op.id = bad.clone();
        assert!(build(vec![("m".into(), model(0))], vec![op]).is_err());
        let mut op = base();
        op.sent_keys = vec![bad.clone()];
        assert!(build(vec![("m".into(), model(0))], vec![op]).is_err());
        assert!(build(vec![(bad, model(0))], vec![]).is_err());
    }
}
#[test]
fn exact_configuration_limits_and_one_over_are_checked() {
    let models: Vec<_> = (0..64).map(|i| (format!("m{i}"), model(1))).collect();
    let operations: Vec<_> = (0..64)
        .map(|i| MockOperation {
            id: format!("op{i}"),
            model: format!("m{i}"),
            sent_keys: (0..64).map(|j| format!("k{i}-{j}")).collect(),
            script: MockScript { failed_generations: failures(63), submit_failures: failures(1024), ..Default::default() },
        })
        .collect();
    assert!(build(models.clone(), operations.clone()).is_ok());
    let mut too_many = models.clone();
    too_many.push(("extra".into(), model(1)));
    assert!(build(too_many, vec![]).is_err());
    let mut ops = operations.clone();
    ops.push(operation("extra", &["extra"], MockScript::default()));
    assert!(build(models.clone(), ops).is_err());
    let mut ops = operations.clone();
    ops[0].sent_keys.push("extra".into());
    assert!(build(models.clone(), ops).is_err());
    let mut ops = operations.clone();
    ops[0].script.failed_generations = failures(64);
    assert!(build(models.clone(), ops).is_err());
    let long = "x".repeat(64);
    assert!(
        build(
            vec![(long.clone(), model(0))],
            vec![MockOperation { id: long.clone(), model: long.clone(), sent_keys: vec![long], script: MockScript::default() }]
        )
        .is_ok()
    );
    let mut empty = operation("op", &[], MockScript::default());
    assert!(build(vec![("m".into(), model(0))], vec![empty.clone()]).is_err());
    empty.sent_keys = vec!["a".into(), "a".into()];
    assert!(build(vec![("m".into(), model(0))], vec![empty]).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn concurrent_operations_do_not_share_failures() {
    let script = MockScript { submit_failures: failures(1), ..Default::default() };
    let p = MockProvider::scripted("mock", [("m".into(), model(0))], [operation("a", &["a"], script), operation("b", &["b"], script)]).unwrap();
    let workers: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|key| {
            let p = p.clone();
            std::thread::spawn(move || {
                assert!(block_on(p.submit(&job(key))).is_err());
                assert!(block_on(p.submit(&job(key))).is_ok());
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(p.billed(), 2);
}
#[test]
fn cancelling_an_old_generation_blocks_further_replacements() {
    let p = provider(1, MockScript { failed_generations: Some(MockFailures { count: 2, kind: ErrorKind::JobFailed }), ..Default::default() }, &["a", "b", "c"]);
    let a = queued(&p, "a");
    assert!(matches!(block_on(p.poll(&a)).unwrap(), JobProgress::Failed(_)));
    let b = queued(&p, "b");
    assert!(matches!(block_on(p.poll(&b)).unwrap(), JobProgress::Failed(_)));
    block_on(p.cancel(&a)).unwrap();
    assert!(block_on(p.submit(&job("c"))).is_err());
    assert_eq!(p.billed(), 2);
}

#[test]
fn constructor_stops_at_the_first_over_cap_item_and_diagnostics_are_static() {
    use std::cell::Cell;
    let consumed = Cell::new(0);
    let models = std::iter::from_fn(|| {
        let n = consumed.get();
        consumed.set(n + 1);
        Some((format!("m{n}"), model(0)))
    });
    assert!(MockProvider::scripted("mock", models, []).is_err());
    assert_eq!(consumed.get(), 65);
    let consumed = Cell::new(0);
    let ops = std::iter::from_fn(|| {
        let n = consumed.get();
        consumed.set(n + 1);
        Some(MockOperation { id: format!("op{n}"), model: "m".into(), sent_keys: vec![format!("k{n}")], script: MockScript::default() })
    });
    assert!(MockProvider::scripted("mock", [("m".into(), model(0))], ops).is_err());
    assert_eq!(consumed.get(), 65);
    let secret = "private-secret".repeat(1000);
    let err = MockProvider::scripted(&secret, [("m".into(), model(0))], []).err().unwrap();
    assert_eq!(err.kind, ErrorKind::UnsupportedInput);
    assert!(!err.message.contains("private-secret"));
    assert!(err.message.len() < 100);
    let p = provider(0, MockScript::default(), &["a"]);
    let err = block_on(p.submit(&job(&secret))).unwrap_err();
    assert!(!err.message.contains("private-secret"));
    assert_eq!(p.billed(), 0);
}

#[test]
fn finite_configuration_does_not_retain_caller_spare_string_capacity() {
    let spare = |text: &str| {
        let mut s = String::with_capacity(4096);
        s.push_str(text);
        s
    };
    let p = MockProvider::scripted(
        spare("mock"),
        [(spare("m"), model(1))],
        [MockOperation {
            id: spare("op"),
            model: spare("m"),
            sent_keys: vec![spare("a"), spare("b")],
            script: MockScript { failed_generations: Some(MockFailures { count: 1, kind: ErrorKind::JobFailed }), ..Default::default() },
        }],
    )
    .unwrap();
    let compact = |s: &String| assert!(s.capacity() <= 64, "validated short configuration retained {} bytes of capacity", s.capacity());
    compact(&p.id);
    for name in p.models.keys() {
        compact(name);
    }
    for name in &p.scripts.models {
        compact(name);
    }
    for (name, key) in &p.scripts.keys {
        compact(name);
        compact(&key.model);
        if let Some(prior) = &key.prior {
            compact(prior);
        }
    }
    for key in p.lock().scripted.keys() {
        compact(key);
    }
    // The legacy constructor's ownership contract is unchanged by the bounded API fix.
    let legacy = MockProvider::new("legacy", [(spare("m"), model(0))]);
    assert!(legacy.models.keys().next().unwrap().capacity() >= 4096);
}
