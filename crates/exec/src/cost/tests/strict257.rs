use super::*;

fn metadata(key: &str) -> UsageMetadata<'_> {
    UsageMetadata { at_ms: 1, session: "a", node: NodeId::from_bytes([1; 16]), provider: "t", model: "m", idempotency_key: key }
}

fn ambiguous() -> UsageLog {
    serde_json::from_str(include_str!("../fixtures/legacy-folded-open-reused-actual.json")).unwrap()
}

#[test]
fn immutable_uncertainty_roundtrips_and_new_same_group_money_can_settle() {
    let mut log = ambiguous();
    assert_eq!(log.ownership("t", "old").unwrap(), Some(ChargeOwnership::LegacyUncertain));
    assert_eq!(log.quarantine_entries().count(), 1);
    let original = log.reported_spent("a");
    assert!(log.budget_view("a").has_uncertain_session_charges);
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(10)));
    for cost in [2, 5, 8] {
        let before = log.clone();
        assert!(log.settle("t", "old", usd(cost), Charge::Actual).is_err());
        assert_eq!(log, before);
    }
    log.record(estimate("a", "new", 7)).unwrap();
    log.settle("t", "new", usd(2), Charge::Actual).unwrap();
    assert_eq!(log.reported_spent("a").project.get(&Currency::USD), Some(&usd(12)));
    let wire = serde_json::to_string(&log).unwrap();
    assert_eq!(serde_json::from_str::<UsageLogFile>(&wire).unwrap().open_session_ownership, 2);
    let mut back: UsageLog = serde_json::from_str(&wire).unwrap();
    assert_eq!(log, back);
    assert!(back.release("t", "old").is_err());
    assert_eq!(original.project.get(&Currency::USD), Some(&usd(10)));
}

#[test]
fn metadata_actual_overlay_survives_clone_csv_save_and_ordinary_mutations() {
    let mut log = UsageLog::default();
    let mut first = log.reserve_metadata(metadata("first")).unwrap();
    let mut abandoned = log.reserve_metadata(metadata("gap")).unwrap();
    log.cancel_metadata(&mut abandoned).unwrap();
    log.record(estimate("a", "later", 3)).unwrap();
    let mut last = log.reserve_metadata(metadata("last")).unwrap();
    log.finalize_metadata(&mut last, usd(2)).unwrap();
    log.finalize_metadata(&mut first, Money::new(Currency::EUR, 4)).unwrap();
    assert_eq!(log.metadata_statistics().finalized, 2);
    assert_eq!(log.entries().map(|e| e.idempotency_key.as_str()).collect::<Vec<_>>(), ["first", "later", "last"]);
    assert!(log.to_csv().contains("first"));
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(5)));
    let snapshot = log.clone();
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(log, back);
    assert_eq!(snapshot, back);
    log.release("t", "later").unwrap();
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(2)));
    log.record(estimate("b", "fresh", 1)).unwrap();
    assert_eq!(log.metadata_statistics().finalized, 0, "ordinary admitted transaction moves Actual into base");
    assert_eq!(log.entries().map(|e| e.idempotency_key.as_str()).collect::<Vec<_>>(), ["first", "last", "fresh"]);
    assert_eq!(log.spent("a").project.get(&Currency::EUR), Some(&Money::new(Currency::EUR, 4)));
}

#[test]
fn permits_are_exclusive_live_capabilities_and_failed_finalization_retains_them() {
    let mut log = UsageLog::default();
    let mut permit = log.reserve_metadata(metadata("key")).unwrap();
    assert_eq!(log.reserve_metadata(metadata("key")).unwrap_err().kind, ErrorKind::Busy);
    let mut clone = log.clone();
    assert_eq!(clone.metadata_statistics().held, 0);
    assert_eq!(clone.finalize_metadata(&mut permit, usd(1)), Err(MetadataFinalizeError::LedgerReplaced));
    assert_eq!(log.metadata_statistics().held, 1);
    assert_eq!(log.finalize_metadata(&mut permit, usd(MAX_CHARGE_MICROS + 1)), Err(MetadataFinalizeError::InvalidReportedMoney));
    assert_eq!(log.metadata_statistics().held, 1);
    log.finalize_metadata(&mut permit, usd(7)).unwrap();
    assert_eq!(log.finalize_metadata(&mut permit, usd(7)), Err(MetadataFinalizeError::PermitConsumed));
    drop(permit);
    assert_eq!(log.metadata_statistics().finalized, 1);
    let vacant = log.reserve_metadata(metadata("drop")).unwrap();
    let backing = log.metadata_statistics().backing_bytes;
    drop(vacant);
    assert_eq!(log.metadata_statistics().held, 0);
    assert_eq!(log.metadata_statistics().backing_bytes, backing, "vacant backing remains accounted");
    let again = log.reserve_metadata(metadata("drop")).unwrap();
    drop(again);
}

#[test]
fn marker2_rejects_floor_reduction_and_contradictory_shared_evidence() {
    let log = ambiguous();
    let mut file: UsageLogFile = log.into();
    file.sessions.get_mut("a").unwrap().insert(Currency::USD, usd(4));
    file.sessions.get_mut(EARLIER_SESSIONS).unwrap().insert(Currency::USD, usd(6));
    assert!(UsageLog::try_from(file).is_err());
    let mut file: UsageLogFile = ambiguous().into();
    file.entries.push(estimate("b", "bq", 2));
    file.uncertain_listed.push(u32::try_from(file.entries.len() - 1).unwrap());
    file.project.insert(Currency::USD, usd(12));
    file.sessions.insert("b".into(), BTreeMap::from([(Currency::USD, usd(2))]));
    file.legacy_groups[0].original_project = usd(12);
    file.legacy_groups.push(LegacyGroupEvidence {
        session: "b".into(),
        original_named: usd(2),
        original_earlier: usd(5),
        remaining_earlier_after_exact_migrations: usd(5),
        original_project: usd(12),
        original_open: usd(2),
    });
    assert_eq!(UsageLog::try_from(file.clone()).unwrap().quarantine_entries().count(), 2);
    file.legacy_groups[1].original_earlier = usd(6);
    assert!(UsageLog::try_from(file).unwrap_err().message.contains("contradictory"));
}

fn pinned(count: usize) -> UsageLog {
    let n = u64::try_from(count).unwrap();
    let mut file = UsageLogFile { open_session_ownership: 2, ..UsageLogFile::default() };
    file.project.insert(Currency::USD, usd(n));
    file.sessions.insert("a".into(), BTreeMap::from([(Currency::USD, usd(1))]));
    file.sessions.insert(EARLIER_SESSIONS.into(), BTreeMap::from([(Currency::USD, usd(n - 1))]));
    file.quarantined = (0..count).map(|i| estimate("a", &format!("q{i}"), 1)).collect();
    file.legacy_groups.push(LegacyGroupEvidence {
        session: "a".into(),
        original_named: usd(1),
        original_earlier: usd(n - 1),
        remaining_earlier_after_exact_migrations: usd(n - 1),
        original_project: usd(n),
        original_open: usd(n),
    });
    UsageLog::try_from(file).unwrap()
}

#[test]
fn last_retained_slot_is_held_before_copy_and_competing_writers_cannot_steal_it() {
    let mut log = pinned(MAX_USAGE_ENTRIES + MAX_PENDING - 1);
    let mut permit = log.reserve_metadata(metadata("held")).unwrap();
    assert_eq!(log.metadata_statistics().held, 1);
    let copies = metadata::copied_bytes();
    let state = log.metadata_statistics();
    assert_eq!(log.reserve_metadata(metadata("competing")).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(metadata::copied_bytes(), copies, "capacity refusal precedes all owned metadata text copies");
    assert_eq!(log.metadata_statistics(), state);
    assert_eq!(log.record(estimate("a", "ordinary", 2)).unwrap_err().kind, ErrorKind::Busy);
    log.finalize_metadata(&mut permit, usd(7)).unwrap();
    assert_eq!(log.charge("t", "held").unwrap().cost, usd(7));
    assert_eq!(log.quarantine_entries().count(), MAX_USAGE_ENTRIES + MAX_PENDING - 1);
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(u64::try_from(MAX_USAGE_ENTRIES + MAX_PENDING - 1).unwrap() + 7)));
}

#[test]
fn uncertain_fifo_eviction_never_promotes_or_reuses_the_key() {
    let mut log = ambiguous();
    // Force real text-based FIFO eviction with bounded maximal fields, not a private move.
    for n in 0..(MAX_LISTED_TEXT / (MAX_TEXT * 2) + 2) {
        let mut entry = estimate("b", &format!("{n:0512}"), 0);
        entry.model = "m".repeat(MAX_TEXT);
        entry.status = Charge::Actual;
        log.record(entry).unwrap();
    }
    assert_eq!(log.quarantine_entries().next().unwrap().location, QuarantineLocation::Evicted);
    assert!(log.release("t", "old").is_err());
    assert!(!log.record(estimate("a", "old", 5)).unwrap());
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(back.ownership("t", "old").unwrap(), Some(ChargeOwnership::LegacyUncertain));
    assert_eq!(back.quarantine_entries().count(), 1);
}

#[test]
fn zero_owner_migration_precedes_other_group_uncertainty_classification() {
    let mut file: UsageLogFile = serde_json::from_str(include_str!("../fixtures/legacy-folded-open.json")).unwrap();
    file.entries.push(estimate("b", "owned", 3));
    file.project.insert(Currency::USD, usd(8));
    file.sessions.insert("b".into(), BTreeMap::from([(Currency::USD, usd(3))]));
    let mut log = UsageLog::try_from(file).unwrap();
    assert_eq!(log.quarantine_entries().count(), 0, "earlier funds were all necessarily transferred to zero owner");
    log.release("t", "owned").unwrap();
    log.release("t", "old").unwrap();
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(0)));
}

#[test]
fn accepted_owned_strings_and_vectors_do_not_retain_callers_unused_capacity() {
    let mut entry = estimate("short", "short-key", 1);
    entry.model.reserve(1024 * 1024);
    entry.provider.reserve(1024 * 1024);
    let mut log = UsageLog::default();
    log.record(entry).unwrap();
    let row = log.charge("t", "short-key").unwrap();
    assert!(row.model.capacity() <= MAX_TEXT && row.provider.capacity() <= MAX_TEXT);
    let mut file: UsageLogFile = log.into();
    file.entries.reserve(10000);
    file.entries[0].session.reserve(1024 * 1024);
    let log = UsageLog::try_from(file).unwrap();
    assert!(log.entries.capacity() < 10000);
    assert!(log.entries().next().unwrap().session.capacity() <= MAX_TEXT);
}

#[test]
fn shell_limit_is_not_a_blanket_freeze_and_abandoned_slots_are_reusable() {
    let mut log = UsageLog::default();
    let mut holds: Vec<_> = (0..MAX_METADATA_SHELLS).map(|i| log.reserve_metadata(metadata(&format!("hold{i}"))).unwrap()).collect();
    let before = log.metadata_statistics();
    let copied = metadata::copied_bytes();
    assert_eq!(log.reserve_metadata(metadata("over")).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(log.metadata_statistics(), before);
    assert_eq!(metadata::copied_bytes(), copied);
    log.record(estimate("unrelated", "still-works", 1)).unwrap();
    drop(holds.pop().unwrap());
    let slot = log.reserve_metadata(metadata("reused")).unwrap();
    assert_eq!(log.metadata_statistics().held, MAX_METADATA_SHELLS);
    drop(slot);
    drop(holds);
    assert_eq!(log.metadata_statistics().held, 0);
}

#[test]
fn retained_text_exact_boundary_and_one_byte_short_precede_owned_copies() {
    let cap = MAX_LISTED_TEXT + MAX_PENDING * 4 * MAX_TEXT;
    assert_eq!(cap % (4 * MAX_TEXT), 0);
    let count = cap / (4 * MAX_TEXT);
    let n = u64::try_from(count).unwrap();
    let name = "a".repeat(MAX_TEXT);
    let provider = "p".repeat(MAX_TEXT);
    let model = "m".repeat(MAX_TEXT);
    let mut file = UsageLogFile { open_session_ownership: 2, ..UsageLogFile::default() };
    file.project.insert(Currency::USD, usd(n));
    file.sessions.insert(name.clone(), [(Currency::USD, usd(1))].into());
    file.sessions.insert(String::new(), [(Currency::USD, usd(n - 1))].into());
    file.quarantined = (0..count)
        .map(|i| UsageEntry {
            at_ms: 0,
            session: name.clone(),
            node: NodeId::from_bytes([1; 16]),
            provider: provider.clone(),
            model: model.clone(),
            idempotency_key: format!("{i:0512}"),
            cost: usd(1),
            status: Charge::Estimate,
        })
        .collect();
    file.legacy_groups.push(LegacyGroupEvidence {
        session: name.into(),
        original_named: usd(1),
        original_earlier: usd(n - 1),
        remaining_earlier_after_exact_migrations: usd(n - 1),
        original_project: usd(n),
        original_open: usd(n),
    });
    // Leave exactly seven bytes: a/t/m/held is seven; a/t/m/large is eight.
    file.quarantined[0].model.truncate(MAX_TEXT - 7);
    let mut log = UsageLog::try_from(file).unwrap();
    let copied = metadata::copied_bytes();
    assert_eq!(log.reserve_metadata(metadata("large")).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(metadata::copied_bytes(), copied);
    let mut exact = log.reserve_metadata(metadata("held")).unwrap();
    assert_eq!(metadata::copied_bytes() - copied, 7);
    log.finalize_metadata(&mut exact, usd(1)).unwrap();
    assert_eq!(log.metadata_statistics().text_bytes, 7);
}

#[test]
fn actual_main_writer_ambiguous_log_loads_but_cannot_release_uncertain_money() {
    let mut writer = UsageLog::default();
    writer.record(estimate("a", "old", 5)).unwrap();
    let mut spent = estimate("closed", "spent", 5);
    spent.status = Charge::Actual;
    writer.record(spent).unwrap();
    for n in 0..MAX_SESSIONS + 2 {
        let mut e = estimate(&format!("other-{n}"), &format!("other-{n}"), 0);
        e.status = Charge::Actual;
        writer.record(e).unwrap();
    }
    assert_eq!(writer.sessions.get(EARLIER_SESSIONS).unwrap().get(&Currency::USD), Some(&usd(5)));
    let wire = serde_json::to_value(&writer).unwrap();
    // Simulate the actual main writer, which predates ownership assertions.
    let mut legacy = wire;
    legacy.as_object_mut().unwrap().remove("open_session_ownership");
    let mut log: UsageLog = serde_json::from_value(legacy).unwrap();
    let before = serde_json::to_value(&log).unwrap();
    assert!(log.release("t", "old").is_err(), "legacy wire cannot distinguish this valid owned case from an unsafe folded case");
    assert_eq!(serde_json::to_value(&log).unwrap(), before);
}

#[test]
fn listed_pagination_merges_actual_overlay_with_sequence_gaps_and_omits_holds() {
    let mut log = UsageLog::default();
    let mut first = log.reserve_metadata(metadata("first")).unwrap();
    let mut gap = log.reserve_metadata(metadata("gap")).unwrap();
    log.cancel_metadata(&mut gap).unwrap();
    log.record(estimate("a", "later", 3)).unwrap();
    let mut last = log.reserve_metadata(metadata("last")).unwrap();
    // Reserve before finalization so the test observes live overlays, not a later flush.
    let held = log.reserve_metadata(metadata("held")).unwrap();
    log.finalize_metadata(&mut last, usd(2)).unwrap();
    log.finalize_metadata(&mut first, usd(4)).unwrap();
    assert_eq!(log.metadata_statistics().finalized, 2);
    assert_eq!(log.listed(), 3);
    let whole = log.to_csv();
    let mut pages = Vec::new();
    for n in 0..3 {
        let mut page = Vec::new();
        log.write_csv_rows(&mut page, n, 1).unwrap();
        let page = String::from_utf8(page).unwrap();
        assert_eq!(page.lines().count(), 2);
        pages.push(page.lines().nth(1).unwrap().to_owned());
    }
    assert_eq!(whole.lines().skip(1).collect::<Vec<_>>(), pages.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(pages[0].ends_with(",first") && pages[1].ends_with(",later") && pages[2].ends_with(",last"));
    let mut end = Vec::new();
    log.write_csv_rows(&mut end, usize::MAX, usize::MAX).unwrap();
    assert_eq!(String::from_utf8(end).unwrap().lines().count(), 1);
    drop(held);
}
