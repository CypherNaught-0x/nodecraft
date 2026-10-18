use super::*;
use nodecraft_types::{ProviderInstanceId, ProviderTarget};
fn usd(n: u64) -> Money {
    Money::new(Currency::USD, n)
}
fn target(n: u8) -> ProviderTarget {
    ProviderTarget::named("local", ProviderInstanceId::from_bytes([n; 16])).unwrap()
}
fn entry(target: &ProviderTarget, key: &str) -> UsageEntry {
    UsageEntry {
        at_ms: 1,
        session: "a".into(),
        node: NodeId::from_bytes([1; 16]),
        provider: target.adapter().into(),
        instance: target.instance(),
        model: "m".into(),
        cost: usd(7),
        idempotency_key: key.into(),
        status: Charge::Estimate,
    }
}
fn metadata<'a>(target: &'a ProviderTarget, key: &'a str) -> UsageMetadata<'a> {
    UsageMetadata {
        at_ms: 1,
        session: "a",
        node: NodeId::from_bytes([1; 16]),
        provider: target.adapter(),
        instance: target.instance(),
        model: "m",
        idempotency_key: key,
    }
}
#[test]
fn same_adapter_key_different_instances_have_independent_money_and_default_wrappers() {
    let mut log = UsageLog::default();
    let default = ProviderTarget::default_instance("local").unwrap();
    for t in [&default, &target(0), &target(1)] {
        assert!(log.record(entry(t, "same")).unwrap());
        assert!(!log.record(entry(t, "same")).unwrap());
    }
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(21)));
    log.release("local", "same").unwrap();
    assert!(log.release("local", "same").is_err());
    assert_eq!(log.charge_target(&target(0), "same").unwrap().cost, usd(7));
    log.settle_target(&target(0), "same", usd(3), Charge::Actual).unwrap();
    log.release_target(&target(1), "same").unwrap();
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(3)));
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(back, log);
    let alias = ProviderTarget::default_instance(&target(0).lane_name().unwrap()).unwrap();
    assert!(log.record(entry(&alias, "same")).unwrap());
}
#[test]
fn first_named_hold_sticks_after_drop_clone_and_wire_and_rejects_legacy_marker() {
    let mut log = UsageLog::default();
    let named = target(0);
    let permit = log.reserve_metadata(metadata(&named, "hold")).unwrap();
    drop(permit);
    let file: UsageLogFile = log.clone().into();
    assert_eq!(file.provider_identity_version, 1);
    assert_eq!(file.open_session_ownership, 1);
    assert!(file.entries.is_empty());
    let back = UsageLog::try_from(file.clone()).unwrap();
    assert_eq!(UsageLogFile::from(back).provider_identity_version, 1);
    let mut file = file;
    file.open_session_ownership = 0;
    assert!(UsageLog::try_from(file).is_err());
    let mut file: UsageLogFile = log.into();
    file.provider_identity_version = 2;
    assert!(UsageLog::try_from(file).is_err());
}
#[test]
fn scoped_wire_and_default_uncertainty_are_separate_axes() {
    let mut log: UsageLog = serde_json::from_str(include_str!("fixtures/legacy-folded-open-reused-actual.json")).unwrap();
    assert_eq!(log.ownership("t", "old").unwrap(), Some(ChargeOwnership::LegacyUncertain));
    log.record(entry(&target(0), "scoped")).unwrap();
    let file: UsageLogFile = log.clone().into();
    assert_eq!((file.provider_identity_version, file.open_session_ownership), (1, 2));
    let mut back = UsageLog::try_from(file.clone()).unwrap();
    assert!(back.release("t", "old").is_err());
    back.release_target(&target(0), "scoped").unwrap();
    let mut legacy = file.clone();
    legacy.provider_identity_version = 0;
    assert!(UsageLog::try_from(legacy).is_err());
    let mut uncertain = file.clone();
    let n = uncertain.entries.iter().position(|e| e.instance.is_some()).unwrap();
    uncertain.uncertain_listed.push(n as u32);
    assert!(UsageLog::try_from(uncertain).is_err());
    let mut under = file;
    under.sessions.get_mut("a").unwrap().insert(Currency::USD, usd(7));
    under.sessions.get_mut(EARLIER_SESSIONS).unwrap().insert(Currency::USD, usd(10));
    assert!(UsageLog::try_from(under).is_err());
}
#[test]
fn retired_wire_preserves_default_bytes_and_tagged_scoped_fifo() {
    let default = retired_id("local", "same");
    let named = retired_target_id("local", Some(ProviderInstanceId::from_bytes([0; 16])), "same");
    assert_ne!(default, named);
    let mut log = UsageLog { retired: retirement::Store::admit([default].into_iter()).unwrap(), ..Default::default() };
    let old: UsageLogFile = log.clone().into();
    assert_eq!(old.retired, [hex(&default)]);
    assert!(old.retired_targets.is_empty());
    assert_eq!(old.retired[0].len(), 32);
    log.provider_identity_version = 1;
    let stage = log.retired.prepare(std::iter::once(named)).unwrap();
    stage.commit(&mut log.retired);
    let scoped: UsageLogFile = log.clone().into();
    assert!(scoped.retired.is_empty());
    assert_eq!(scoped.retired_targets.iter().map(|w| (w.tag, w.digest.len())).collect::<Vec<_>>(), [(0, 32), (1, 64)]);
    assert_eq!(UsageLog::try_from(scoped.clone()).unwrap(), log);
    for tag in [2, 255] {
        let mut bad = scoped.clone();
        bad.retired_targets[0].tag = tag;
        assert!(UsageLog::try_from(bad).is_err());
    }
    let mut dup = scoped;
    dup.retired_targets.push(dup.retired_targets[0].clone());
    assert!(UsageLog::try_from(dup).is_err());
}
#[test]
fn legacy_csv_rejects_entire_selected_stream_before_header_or_partial_page() {
    let mut log = UsageLog::default();
    let default = ProviderTarget::default_instance("local").unwrap();
    log.record(entry(&default, "first")).unwrap();
    log.record(entry(&target(0), "second")).unwrap();
    let mut out = Vec::new();
    assert!(log.write_csv_rows(&mut out, 0, 2).is_err());
    assert!(out.is_empty());
    log.write_csv_rows(&mut out, 0, 1).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("first"));
    let mut out = Vec::new();
    log.write_scoped_csv_rows(&mut out, 0, 2).unwrap();
    let csv = String::from_utf8(out).unwrap();
    assert!(csv.contains("provider_instance"));
    assert!(csv.contains(&ProviderInstanceId::from_bytes([0; 16]).to_string()));
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scoped_finalization_really_allocates_zero_including_version_publication() {
    let mut log = UsageLog::default();
    let t = target(0);
    let mut permit = log.reserve_metadata(metadata(&t, "key")).unwrap();
    assert_eq!(log.provider_identity_version, 1);
    let measured = allocation_counter::measure(|| log.finalize_metadata(&mut permit, usd(3)).unwrap());
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(log.charge_target(&t, "key").unwrap().instance, t.instance());
}

#[test]
fn live_digest_accelerators_cannot_grant_cross_tuple_ownership_or_permit_reuse() {
    let a = target(0);
    let b = target(1);
    let mut log = UsageLog::default();
    log.record(entry(&a, "same")).unwrap();
    let seq = log.keys.remove(&ownership::identity_target(a.adapter(), a.instance(), "same")).unwrap();
    log.keys.insert(ownership::identity_target(b.adapter(), b.instance(), "same"), seq);
    assert_eq!(log.ownership_target(&b, "same").unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log.release_target(&b, "same").unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log.reserve_metadata(metadata(&b, "same")).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log.entries.front().unwrap().instance, a.instance());
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(7)));
    let mut log = UsageLog::default();
    let mut permit = log.reserve_metadata(metadata(&a, "held")).unwrap();
    log.metadata.replace_first_digest_for_test(ownership::identity_target(b.adapter(), b.instance(), "held"));
    let before = metadata::copied_bytes();
    assert_eq!(log.reserve_metadata(metadata(&b, "held")).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(metadata::copied_bytes(), before);
    assert_eq!(log.metadata_statistics().held, 1);
    log.finalize_metadata(&mut permit, usd(3)).unwrap();
    assert_eq!(log.entries().next().unwrap().instance, a.instance());
}
#[test]
fn retired_membership_is_dedup_only_never_a_settlement_or_release_capability() {
    let t = target(0);
    let mut log = UsageLog {
        provider_identity_version: 1,
        retired: retirement::Store::admit(std::iter::once(retired_target_id(t.adapter(), t.instance(), "forgotten"))).unwrap(),
        ..Default::default()
    };
    assert!(log.has_recorded_target(&t, "forgotten").unwrap());
    assert!(log.charge_target(&t, "forgotten").is_none());
    assert_eq!(log.ownership_target(&t, "forgotten").unwrap(), None);
    assert!(log.release_target(&t, "forgotten").is_err());
    assert!(log.settle_target(&t, "forgotten", usd(0), Charge::Actual).is_err());
    assert!(log.reserve_metadata(metadata(&t, "forgotten")).is_err());
    assert!(log.reported_spent("a").project.is_empty());
}

#[test]
fn retirement_failures_keep_record_hold_and_pending_settlement_wholly_unpublished() {
    let default = ProviderTarget::default_instance("local").unwrap();
    let mut file = UsageLogFile { open_session_ownership: 1, ..Default::default() };
    file.entries = (0..MAX_USAGE_ENTRIES)
        .map(|n| {
            let mut e = entry(&default, &format!("k{n}"));
            e.cost = usd(0);
            e.status = Charge::Actual;
            e
        })
        .collect();
    file.project.insert(Currency::USD, usd(0));
    file.sessions.insert("a".into(), [(Currency::USD, usd(0))].into());
    let mut log = UsageLog::try_from(file).unwrap();
    let original = log.clone();
    let backing = (log.entries.capacity(), log.entry_order.capacity(), log.retired_logical_bytes(), log.metadata_statistics().backing_bytes);
    for boundary in 0..6 {
        retirement::fail_at(Some(boundary));
        assert!(log.record(entry(&target(0), "fresh")).is_err());
        retirement::fail_at(None);
        assert_eq!(log, original);
        assert_eq!((log.entries.capacity(), log.entry_order.capacity(), log.retired_logical_bytes(), log.metadata_statistics().backing_bytes), backing);
        assert_eq!(log.provider_identity_version, 0);
        retirement::fail_at(Some(boundary));
        assert!(log.reserve_metadata(metadata(&target(0), "held")).is_err());
        retirement::fail_at(None);
        assert_eq!(log, original);
        assert_eq!((log.entries.capacity(), log.entry_order.capacity(), log.retired_logical_bytes(), log.metadata_statistics().backing_bytes), backing);
        assert_eq!(log.provider_identity_version, 0);
    }
    let t = target(0);
    let mut file = UsageLogFile { provider_identity_version: 1, open_session_ownership: 1, ..Default::default() };
    file.pending.push(entry(&t, "pending"));
    file.project.insert(Currency::USD, usd(7));
    file.sessions.insert("a".into(), [(Currency::USD, usd(7))].into());
    let mut log = UsageLog::try_from(file).unwrap();
    let old = log.clone();
    for boundary in 0..6 {
        retirement::fail_at(Some(boundary));
        assert!(log.settle_target(&t, "pending", usd(3), Charge::Actual).is_err());
        retirement::fail_at(None);
        assert_eq!(log, old);
        assert_eq!(log.retired_logical_bytes(), old.retired_logical_bytes());
    }
}

#[test]
fn full_retired_wire_window_keeps_compact_dto_and_exact_next_victim() {
    let mut log = UsageLog { provider_identity_version: 1, ..Default::default() };
    let keys = (0..MAX_RETIRED_KEYS).map(|n| {
        let t = if n % 2 == 0 { ProviderTarget::default_instance("local").unwrap() } else { target(0) };
        retired_target_id(t.adapter(), t.instance(), &format!("k{n}"))
    });
    log.retired = retirement::Store::admit(keys).unwrap();
    let file: UsageLogFile = log.clone().into();
    assert_eq!(file.retired_targets.len(), MAX_RETIRED_KEYS);
    assert!(file.retired_targets.capacity() <= MAX_RETIRED_KEYS);
    assert!(file.retired_targets.iter().all(|w| w.digest.capacity() <= if w.tag == 0 { 32 } else { 64 }));
    assert!(serde_json::to_vec(&file.retired_targets).unwrap().len() <= 96 * MAX_RETIRED_KEYS + 128);
    let mut back = UsageLog::try_from(file).unwrap();
    assert_eq!(back.retired.fifo, log.retired.fifo);
    let new = retired_target_id("local", target(0).instance(), "newest");
    let stage = back.retired.prepare(std::iter::once(new)).unwrap();
    stage.commit(&mut back.retired);
    assert_eq!(back.retired.fifo.front(), log.retired.fifo.get(1));
    assert_eq!(back.retired.fifo.back(), Some(&new));
    assert_eq!(back.retired.fifo.len(), MAX_RETIRED_KEYS);
}
