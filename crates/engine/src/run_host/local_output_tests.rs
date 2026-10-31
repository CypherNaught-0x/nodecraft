//! A local job's outputs are taken before what it staged is imported (#254), so a failed import
//! leaves nothing counted in the run's held outputs.
#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use nodecraft_exec::driver::{CancelToken, JobRunner};
use nodecraft_exec::run::{LOCAL_LANE, SourceRunner};
use nodecraft_exec::scheduler::{Lane, Outcome, Start};
use nodecraft_types::Value;

/// A runner holding one finished source job's output, `JobId(1)`.
fn finished() -> SourceRunner {
    let runner = SourceRunner::default();
    runner.register(JobId(1), JobSpec::Source { node: NodeId::from_bytes([1; 16]), output: "out".into(), value: Value::Text("made".into()) });
    let start = Start { job: JobId(1), lane: Lane::new(LOCAL_LANE), attempt: 1, idempotency_key: "k".into() };
    assert!(matches!(futures::executor::block_on(runner.run(start, CancelToken::default())), Outcome::Done));
    assert!(runner.outputs.held_bytes() > 0);
    runner
}

#[test]
fn a_local_job_whose_import_fails_leaves_nothing_held() {
    let runner = finished();
    let error = local_outputs(&runner.outputs, JobId(1), || Err("the disk is full".into())).unwrap_err();
    assert_eq!(error, "the disk is full");
    assert_eq!(runner.outputs.held_bytes(), 0, "taken even though the import failed");

    let runner = finished();
    let outputs = local_outputs(&runner.outputs, JobId(1), || Ok(())).unwrap();
    assert_eq!(outputs, vec![("out".to_owned(), Value::Text("made".into()))]);
    assert_eq!(runner.outputs.held_bytes(), 0);
}
