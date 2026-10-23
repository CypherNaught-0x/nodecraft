//! Each project's own total (#847).
use super::*;

const A: ProjectId = ProjectId::from_bytes([1; 16]);
const B: ProjectId = ProjectId::from_bytes([2; 16]);

fn charge_for(project: Option<ProjectId>, key: &str, micros: u64, status: Charge) -> UsageEntry {
    UsageEntry { project, ..charge(key, micros, status) }
}

fn this_project(log: &UsageLog, project: ProjectId) -> Option<Money> {
    log.spent_in("now", Some(project)).this_project.get(&Currency::USD).copied()
}

#[test]
fn a_project_cap_sees_only_its_own_charges_and_the_total_counts_every_one() {
    let (project, plan) = setup();
    let canvas = project.canvases().get(&C).unwrap();
    let full = quote(&plan, canvas, &kinds(), &catalog()).unwrap();
    let priced = Quote { steps: full.steps.iter().filter(|s| s.per_job.is_ok()).cloned().collect(), totals: full.totals.clone() };
    let mut log = UsageLog::default();
    log.record(charge_for(Some(A), "a1", 400_000, Charge::Actual)).unwrap();
    log.record(charge_for(Some(A), "a2", 100_000, Charge::Estimate)).unwrap();
    log.record(charge_for(None, "old", 50_000, Charge::Actual)).unwrap();
    assert_eq!(this_project(&log, A), Some(usd(500_000)));
    assert_eq!(this_project(&log, B), None, "B has spent nothing");
    assert_eq!(log.spent_in("now", Some(B)).project.get(&Currency::USD), Some(&usd(550_000)), "the total counts both");
    assert!(log.spent("now").this_project.is_empty(), "no project asked about");

    // A 0.55 cap per project: A (0.50 + 0.12) is over it, B is not; the total cap still sees A.
    let cap = Budgets { this_project: Some(usd(550_000)), ..Budgets::default() };
    assert_eq!(
        check(&priced, &cap, &log.spent_in("now", Some(A))).unwrap().blocks,
        vec![Block::Over { scope: Scope::ThisProject, node: None, limit: usd(550_000), would_be: usd(620_000) }]
    );
    assert_eq!(
        check(&priced, &cap, &log.spent_in("now", Some(A))).unwrap().blocks.first().unwrap().message(),
        "This would exceed the project budget: 0.62 USD of 0.55 USD."
    );
    assert!(check(&priced, &cap, &log.spent_in("now", Some(B))).unwrap().blocks.is_empty());
    let total = Budgets { project: Some(usd(600_000)), ..Budgets::default() };
    assert!(matches!(check(&priced, &total, &log.spent_in("now", Some(B))).unwrap().blocks.as_slice(), [Block::Over { scope: Scope::Project, .. }]));

    // Settling and releasing move the project's total with the overall one.
    log.settle("fal", "a2", usd(30_000), Charge::Actual).unwrap();
    assert_eq!(this_project(&log, A), Some(usd(430_000)));
    log.record(charge_for(Some(B), "b1", 20_000, Charge::Estimate)).unwrap();
    log.release("fal", "b1").unwrap();
    assert_eq!(this_project(&log, B), Some(usd(0)));
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(480_000)));
}

#[test]
fn project_totals_round_trip_with_finalized_metadata_and_the_csv_names_the_project() {
    let mut log = UsageLog::default();
    log.record(charge_for(Some(A), "a1", 7, Charge::Actual)).unwrap();
    let mut held = log
        .reserve_metadata(UsageMetadata {
            project: Some(B),
            instance: None,
            at_ms: 1,
            session: "now",
            node: id(2),
            provider: "fal",
            model: "m",
            idempotency_key: "b1",
        })
        .unwrap();
    log.finalize_metadata(&mut held, usd(5)).unwrap();
    assert_eq!(this_project(&log, B), Some(usd(5)), "a finalized actual counts before it is flushed");
    let wire = serde_json::to_string(&log).unwrap();
    let file: UsageLogFile = serde_json::from_str(&wire).unwrap();
    assert_eq!(file.projects.get(&B).and_then(|t| t.get(&Currency::USD)), Some(&usd(5)));
    let back: UsageLog = serde_json::from_str(&wire).unwrap();
    assert_eq!(back, log);
    assert_eq!((this_project(&back, A), this_project(&back, B)), (Some(usd(7)), Some(usd(5))));
    let csv = log.to_csv();
    assert!(csv.lines().nth(1).unwrap().ends_with(&format!(",a1,{A}")), "{csv}");
    // A log with no project totals writes none, so older logs keep their shape.
    let mut plain = UsageLog::default();
    plain.record(charge("k", 1, Charge::Actual)).unwrap();
    assert!(!serde_json::to_string(&plain).unwrap().contains("\"projects\""));
}

#[test]
fn loading_raises_a_short_project_total_and_refuses_one_past_the_total() {
    let mut log = UsageLog::default();
    log.record(charge_for(Some(A), "a1", 9, Charge::Actual)).unwrap();
    let mut file = serde_json::to_value(&log).unwrap();
    // A total that doesn't cover its listed charges is raised to cover them.
    file["projects"] = serde_json::json!({});
    let back: UsageLog = serde_json::from_value(file.clone()).unwrap();
    assert_eq!(this_project(&back, A), Some(usd(9)));
    // One larger than everything recorded is refused.
    let mut over = serde_json::to_value(&log).unwrap();
    over["projects"][A.to_string()]["USD"]["micros"] = serde_json::json!(10);
    let wire = serde_json::to_string(&over).unwrap();
    assert!(serde_json::from_str::<UsageLog>(&wire).is_err(), "{wire}");
}

#[test]
fn past_the_project_limit_the_least_recently_charged_total_is_forgotten() {
    let mut log = UsageLog::default();
    let project = |n: usize| {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
        ProjectId::from_bytes(bytes)
    };
    log.record(charge_for(Some(project(0)), "open", 3, Charge::Estimate)).unwrap();
    for n in 1..=MAX_PROJECTS {
        log.record(charge_for(Some(project(n)), &format!("k{n}"), 1, Charge::Actual)).unwrap();
    }
    assert_eq!(this_project(&log, project(0)), None, "the oldest project's total was forgotten");
    assert_eq!(this_project(&log, project(MAX_PROJECTS)), Some(usd(1)));
    let total = u64::try_from(MAX_PROJECTS).unwrap() + 3;
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(total)), "its charges still count in the total");
    // Releasing its open charge takes nothing from a total that is gone, and brings none back.
    log.release("fal", "open").unwrap();
    assert_eq!(this_project(&log, project(0)), None);
    // The log still saves and loads at the limit.
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(back, log);
}
