//! Model requests a run couldn't send, found before its cost is accepted (#307). Graph
//! validation checks each value alone; a model call's parameters and inputs are admitted
//! together (aggregate values, flattened inputs, representation bytes). This runs that same
//! admission over every job the plan already knows, so `run.quote` shows the run as blocked
//! and `run.start` refuses it. Jobs waiting on upstream results are checked when they are
//! converted, as every job still is.

use crate::generate::{MediaSource, RequestCheck, check_request};
use nodecraft_exec::planner::RunPlan;
use nodecraft_types::{AssetRef, Error, NodeId};
use serde_json::{Value, json};
use std::sync::Arc;

/// Jobs checked per quote; past this the rest are left to runtime admission. Each check is
/// bounded by the request limits, so this bounds the quote's work.
const MAX_CHECKED: usize = 4096;

/// A step's largest request among its known jobs (by representation bytes), or its first refused one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub node: NodeId,
    pub jobs: usize,
    pub check: RequestCheck,
}

impl Request {
    /// A fixed-form reason: the admission's own static message and the counts, no values.
    pub fn reason(&self, owner: NodeId) -> Option<String> {
        let refusal = self.check.refusal.as_ref()?;
        Some(format!(
            "node {owner} [request_too_large]: {} (refused after counting {} of {} values and {} of {} inputs); send fewer or smaller parameters and inputs",
            refusal.message, self.check.values, self.check.value_limit, self.check.inputs, self.check.input_limit
        ))
    }

    pub fn json(&self, owner: NodeId) -> Value {
        let c = &self.check;
        json!({
            "node": owner,
            "jobs": self.jobs,
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

/// Each step of `plan` with known model calls, and its worst request.
pub(super) fn requests(plan: &RunPlan, source: &dyn MediaSource) -> Vec<Request> {
    let source = Known(source);
    let mut found = Vec::new();
    let mut checked = 0usize;
    for step in &plan.steps {
        let Some(jobs) = &step.jobs else { continue };
        let mut worst: Option<Request> = None;
        let mut count = 0usize;
        for job in jobs.iter().filter(|job| matches!(job, nodecraft_nodes::JobSpec::Generate { .. })) {
            if checked >= MAX_CHECKED {
                break;
            }
            checked += 1;
            count += 1;
            let check = check_request(job, Some(&source));
            let worse = match &worst {
                None => true,
                Some(prior) => prior.check.refusal.is_none() && (check.refusal.is_some() || check.bytes > prior.check.bytes),
            };
            if worse {
                worst = Some(Request { node: step.node, jobs: 0, check });
            }
        }
        if let Some(mut worst) = worst {
            worst.jobs = count;
            found.push(worst);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_exec::planner::Step;
    use nodecraft_nodes::{GenerateInput, JobSpec, ModeId};
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
        let found = requests(&plan(vec![step(1, Some(vec![job(1, half)])), step(2, Some(vec![both]))]), &NoMedia);
        assert_eq!(found.len(), 2);
        assert!(found[0].reason(node(1)).is_none(), "one half-limit list is admitted");
        let reason = found[1].reason(node(2)).unwrap();
        assert!(reason.contains("[request_too_large]") && reason.contains("of 4096 values"), "{reason}");
        assert_eq!(found[1].json(node(2))["refused"], serde_json::json!(found[1].check.refusal.as_ref().unwrap().message));
        assert_eq!(found[0].json(node(1))["refused"], serde_json::Value::Null);
    }

    #[test]
    fn deferred_and_non_model_steps_are_left_to_runtime() {
        let action = JobSpec::Action { node: node(3), action: "a".into(), params: Default::default(), inputs: vec![], output: "o".into() };
        let found = requests(&plan(vec![step(1, None), step(3, Some(vec![action]))]), &NoMedia);
        assert!(found.is_empty(), "a step waiting on upstream results is checked when it is converted");
    }

    #[test]
    fn a_fanned_step_reports_its_first_refused_item_or_its_largest() {
        let found = requests(&plan(vec![step(1, Some(vec![job(1, 1), job(1, 100), job(1, 10)]))]), &NoMedia);
        assert_eq!((found[0].jobs, found[0].check.values), (3, 102));
        let found = requests(&plan(vec![step(1, Some(vec![job(1, 1), job(1, 5000), job(1, 4000)]))]), &NoMedia);
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
        let found = requests(&plan(vec![step(1, Some(vec![missing]))]), &NoMedia);
        assert_eq!(found[0].check.refusal, None, "a missing asset is not a request too large");
        assert_eq!(found[0].check.media_bytes, 0);
    }

    #[test]
    fn checking_stops_at_its_cap() {
        let found = requests(&plan(vec![step(1, Some(vec![job(1, 0); MAX_CHECKED + 5]))]), &NoMedia);
        assert_eq!(found[0].jobs, MAX_CHECKED);
    }
}
