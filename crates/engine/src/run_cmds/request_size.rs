//! Model requests a run couldn't send, found before its cost is accepted (#307). Graph
//! validation checks each value alone; a model call's parameters and inputs are admitted
//! together (aggregate values, flattened inputs, representation bytes). This runs that same
//! admission over every job the plan already knows, so `run.quote` shows the run as blocked
//! and `run.start` refuses it. Jobs waiting on upstream results are checked when they are
//! converted, as every job still is.

use crate::generate::{MediaSource, RequestCheck, check_request};
use nodecraft_exec::planner::RunPlan;
use nodecraft_nodes::JobSpec;
use nodecraft_types::{AssetRef, Error, NodeId};
use serde_json::{Value, json};
use std::sync::Arc;

/// Jobs checked per quote; past this the rest are left to runtime admission. Each check is
/// bounded by the request limits, so this bounds the quote's work. Every step's first job is
/// checked before any step's second, so a wide fan-out can't hide a later step.
const MAX_CHECKED: usize = 4096;

/// A step's largest request among its known jobs (by representation bytes), or its first refused one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub node: NodeId,
    /// Jobs checked, and the step's model calls; fewer checked means the cap was reached.
    pub jobs: usize,
    pub total: usize,
    pub check: RequestCheck,
}

impl Request {
    /// A fixed-form reason: the admission's own static message and the counts, no values.
    pub fn reason(&self, owner: NodeId) -> Option<String> {
        let refusal = self.check.refusal.as_ref()?;
        Some(format!(
            "node {owner} [request_refused]: {} (refused after counting {} of {} values, {} of {} inputs and {} of {} bytes); change or reduce the parameters and inputs sent in one call",
            refusal.message, self.check.values, self.check.value_limit, self.check.inputs, self.check.input_limit, self.check.bytes, self.check.byte_limit
        ))
    }

    pub fn json(&self, owner: NodeId) -> Value {
        let c = &self.check;
        json!({
            "node": owner,
            "jobs": self.jobs,
            "total_jobs": self.total,
            "values": c.values, "value_limit": c.value_limit,
            "inputs": c.inputs, "input_limit": c.input_limit,
            "bytes": c.bytes, "byte_limit": c.byte_limit,
            "media_bytes": c.media_bytes, "media_limit": c.media_limit,
            "refused": c.refusal.as_ref().map(|e| e.message.clone()),
        })
    }
}

/// Sizes for the check: an asset whose size can't be found counts as empty here and is refused
/// by the job itself when it runs, as [`super::media_size`] leaves it.
struct Known<'a>(&'a dyn MediaSource);
impl MediaSource for Known<'_> {
    fn size(&self, asset: &AssetRef) -> Result<u64, Error> {
        Ok(self.0.size(asset).unwrap_or(0))
    }
    fn read(&self, asset: &AssetRef, limit: u64) -> Result<Arc<[u8]>, Error> {
        self.0.read(asset, limit)
    }
}

/// Each step of `plan` with known model calls, and its worst request; and whether every known
/// call was checked.
pub(super) fn requests(plan: &RunPlan, source: &dyn MediaSource) -> (Vec<Request>, bool) {
    let source = Known(source);
    let steps: Vec<(NodeId, Vec<&JobSpec>)> = plan
        .steps
        .iter()
        .filter_map(|step| Some((step.node, step.jobs.as_ref()?.iter().filter(|job| matches!(job, JobSpec::Generate { .. })).collect::<Vec<_>>())))
        .filter(|(_, jobs)| !jobs.is_empty())
        .collect();
    let mut found: Vec<Option<Request>> = vec![None; steps.len()];
    let mut checked = 0usize;
    // Round one takes each step's first job, round two the rest in order.
    for round in [0..1, 1..usize::MAX] {
        for ((node, jobs), worst) in steps.iter().zip(found.iter_mut()) {
            for job in jobs.get(round.start..).unwrap_or_default().iter().take(round.end.saturating_sub(round.start)) {
                if checked >= MAX_CHECKED {
                    break;
                }
                checked += 1;
                let check = check_request(job, Some(&source));
                match worst {
                    None => *worst = Some(Request { node: *node, jobs: 1, total: jobs.len(), check }),
                    Some(request) => {
                        request.jobs += 1;
                        if request.check.refusal.is_none() && (check.refusal.is_some() || check.bytes > request.check.bytes) {
                            request.check = check;
                        }
                    }
                }
            }
        }
    }
    let complete = found.len() == steps.len() && found.iter().all(|r| r.as_ref().is_some_and(|r| r.jobs == r.total));
    (found.into_iter().flatten().collect(), complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_exec::planner::Step;
    use nodecraft_nodes::{GenerateInput, ModeId};
    use nodecraft_types::{Blake3, ErrorKind, MediaMeta, Value};

    struct NoMedia;
    impl MediaSource for NoMedia {
        fn size(&self, _: &AssetRef) -> Result<u64, Error> {
            Err(Error::new(ErrorKind::NotFound, "no media"))
        }
        fn read(&self, _: &AssetRef, _: u64) -> Result<Arc<[u8]>, Error> {
            Err(Error::new(ErrorKind::Internal, "requests are checked without reading"))
        }
    }
    fn node(n: u8) -> NodeId {
        NodeId::from_bytes([n; 16])
    }
    fn job(n: u8, items: usize) -> JobSpec {
        JobSpec::Generate {
            node: node(n),
            provider: "p".into(),
            model: "m".into(),
            params: [("a".to_owned(), Value::List(vec![Value::Bool(true); items]))].into_iter().collect(),
            inputs: vec![GenerateInput { role: "prompt".into(), value: Value::Text("x".into()) }],
            outputs: vec![],
            instance: None,
        }
    }
    fn step(n: u8, jobs: Option<Vec<JobSpec>>) -> Step {
        Step { node: node(n), kind: "k".into(), mode: ModeId::new("generate").unwrap(), after: vec![], inputs: vec![], jobs, fanout: None }
    }
    fn plan(steps: Vec<Step>) -> RunPlan {
        RunPlan { steps, edges: vec![], inner: Default::default() }
    }

    #[test]
    fn values_that_pass_alone_but_not_together_block_the_step() {
        let half = 2048;
        let mut both = job(1, half);
        if let JobSpec::Generate { params, .. } = &mut both {
            params.insert("b".into(), Value::List(vec![Value::Bool(true); half]));
        }
        let (found, _) = requests(&plan(vec![step(1, Some(vec![job(1, half)])), step(2, Some(vec![both]))]), &NoMedia);
        assert_eq!(found.len(), 2);
        assert!(found[0].reason(node(1)).is_none(), "one half-limit list is admitted");
        let reason = found[1].reason(node(2)).unwrap();
        assert!(reason.contains("[request_refused]") && reason.contains("of 4096 values"), "{reason}");
        assert_eq!(found[1].json(node(2))["refused"], serde_json::json!(found[1].check.refusal.as_ref().unwrap().message));
        assert_eq!(found[0].json(node(1))["refused"], serde_json::Value::Null);
    }

    #[test]
    fn deferred_and_non_model_steps_are_left_to_runtime() {
        let action = JobSpec::Action { node: node(3), action: "a".into(), params: Default::default(), inputs: vec![], output: "o".into() };
        let (found, _) = requests(&plan(vec![step(1, None), step(3, Some(vec![action]))]), &NoMedia);
        assert!(found.is_empty(), "a step waiting on upstream results is checked when it is converted");
    }

    #[test]
    fn a_fanned_step_reports_its_first_refused_item_or_its_largest() {
        let (found, _) = requests(&plan(vec![step(1, Some(vec![job(1, 1), job(1, 100), job(1, 10)]))]), &NoMedia);
        assert_eq!((found[0].jobs, found[0].check.values), (3, 102));
        let (found, _) = requests(&plan(vec![step(1, Some(vec![job(1, 1), job(1, 5000), job(1, 4000)]))]), &NoMedia);
        assert!(found[0].check.refusal.is_some(), "a refused item outranks a larger admitted one");
        assert_eq!(found[0].jobs, 3);
    }

    #[test]
    fn an_asset_of_unknown_size_is_left_to_the_job() {
        let media = Value::Media(AssetRef { hash: Blake3::from_bytes([9; 32]), mime: "image/png".parse().unwrap(), meta: MediaMeta::default() });
        let mut missing = job(1, 0);
        if let JobSpec::Generate { inputs, .. } = &mut missing {
            inputs.push(GenerateInput { role: "image".into(), value: media });
        }
        let (found, _) = requests(&plan(vec![step(1, Some(vec![missing]))]), &NoMedia);
        assert_eq!(found[0].check.refusal, None, "a missing asset is not a request too large");
        assert_eq!(found[0].check.media_bytes, 0);
    }

    #[test]
    fn checking_stops_at_its_cap_after_every_steps_first_job() {
        let (found, complete) = requests(&plan(vec![step(1, Some(vec![job(1, 0); MAX_CHECKED + 5]))]), &NoMedia);
        assert_eq!((found[0].jobs, found[0].total, complete), (MAX_CHECKED, MAX_CHECKED + 5, false));
        // A wide fan-out first can't hide a later step's refused request.
        let mut both = job(2, 2048);
        if let JobSpec::Generate { params, .. } = &mut both {
            params.insert("b".into(), Value::List(vec![Value::Bool(true); 2048]));
        }
        let (found, complete) = requests(&plan(vec![step(1, Some(vec![job(1, 0); MAX_CHECKED])), step(2, Some(vec![both]))]), &NoMedia);
        assert!(!complete);
        assert_eq!((found[0].jobs, found[1].jobs), (MAX_CHECKED - 1, 1));
        assert!(found[1].check.refusal.is_some());
        let (_, complete) = requests(&plan(vec![step(1, Some(vec![job(1, 0); 3]))]), &NoMedia);
        assert!(complete);
    }
}
