//! Actual small Timeline renders with text from the mock provider; no provider network calls.
use super::*;
use crate::action_exec::{
    LocalActions,
    timeline::native::{detach_test_service, service},
};
use nodecraft_exec::run::ActionExecutor;
use nodecraft_format::AssetStore;
use nodecraft_nodes::{
    Registry,
    timeline::{Caption, Clip, Input, Timeline},
};
use nodecraft_providers::{JobInput, Output, ProviderJob, RunLimits, mock::MockProvider, run_to_completion};
use nodecraft_types::{AssetRef, MediaMeta, NodeId, ParamValues, Unsigned, Value};
use std::path::PathBuf;
use std::time::Duration;

fn planned(text: String) -> ParamValues {
    let kinds = Registry::builtins().unwrap();
    let kind = kinds.get(nodecraft_nodes::timeline::ID).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([184; 16]), Default::default(), &Default::default()).unwrap();
    let mut params = kind.validate_instance(&node, true).unwrap().into_visible_values();
    let mut timeline = Timeline::parse(nodecraft_nodes::timeline::EMPTY).unwrap();
    let mut clip = Clip::wired("still".into(), Input::Stills, "source".into(), 0, 0);
    clip.out_us = Some(250_000);
    timeline.clips.push(clip);
    let mut caption = Caption::new("caption".into(), text, 0, 250_000);
    caption.size = 8;
    timeline.captions.push(caption);
    params.insert(nodecraft_nodes::timeline::SPEC_PARAM.into(), Value::Text(timeline.encode().unwrap()));
    params.insert(nodecraft_nodes::timeline::SOURCES_PARAM.into(), Value::List(vec![Value::Text("source".into())]));
    params.insert(nodecraft_nodes::timeline::PORTS_PARAM.into(), Value::List(vec![Value::Text(nodecraft_nodes::timeline::STILLS_INPUT.into())]));
    for (name, value) in [("width", 64), ("height", 48)] {
        params.insert(name.into(), Value::Unsigned(Unsigned(value)));
    }
    params.insert("fps".into(), Value::Text("24".into()));
    params.insert("quality".into(), Value::Text("draft".into()));
    params
}

#[test]
#[ignore = "actual ffmpeg; explicit NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE"]
fn several_timeline_renders_respect_the_shared_staging_quota() {
    let tools =
        (PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap()), PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap()));
    let open = service(&tools, &|| false).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rgba: Vec<u8> = std::iter::repeat_n([17, 31, 55, 255], 16 * 16).flatten().collect();
        let png = nodecraft_media::encode_png(16, 16, &rgba, &Default::default()).unwrap();
        let mut assets = AssetStore::default();
        let hash = assets.insert(&png).unwrap();
        let picture =
            Value::Media(AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta { width: Some(16), height: Some(16), ..Default::default() } });
        let actions = LocalActions::new(assets).with_video_tools(Some(tools.clone()));
        let provider = MockProvider::standard();
        for n in 0..3 {
            let job = ProviderJob {
                model: "mock-text".into(),
                params: Default::default(),
                inputs: vec![JobInput::Text { role: "prompt".into(), text: format!("Shot {n}") }],
                idempotency_key: format!("timeline584-{n}"),
            };
            let done = futures::executor::block_on(run_to_completion(&provider, &job, &NeverCancelled, &InstantClock::default(), RunLimits::default(), |_| {}))
                .unwrap();
            let Some(Output::Text(text)) = done.outputs.into_iter().next() else { panic!("a mock caption") };
            let params = planned(text.chars().take(32).collect());
            let output = actions.execute(nodecraft_nodes::timeline::ID, &params, std::slice::from_ref(&picture));
            if n < 2 {
                let Value::Media(output) = output.unwrap() else { panic!("a rendered Timeline") };
                assert_eq!(output.mime.essence(), "video/webm");
                assert_eq!(output.meta.duration_ms, Some(250));
            } else {
                assert!(output.unwrap_err().contains("staging limit"));
            }
            if n == 1 {
                let mut state = actions.videos.lock().unwrap();
                assert_eq!(state.entries.len(), 2, "distinct captions produce distinct renders");
                assert!(state.retained > 0 && state.retained < MAX_STAGED_BYTES);
                // Approach the production byte ceiling with a small test limit, rather than
                // allocating a GiB. The next actual render cannot fit its read-back reservation.
                state.byte_limit = state.retained + 1;
            }
        }
        let state = actions.videos.lock().unwrap();
        assert_eq!(state.entries.len(), 2);
        assert_eq!((state.reserved, state.pending), (0, 0));
        assert!(state.retained <= state.byte_limit);
        eprintln!("NODECRAFT_TIMELINE_STAGING_BYTES: {} retained in 2 completed renders", state.retained);
        assert_eq!(provider.requests().len(), 3, "captions came from the deterministic mock");
    }));
    // Only a sole owner can shut this service down. Other ignored tests may still be
    // rendering through it, so a shared owner follows its normal lifetime instead.
    detach_test_service(&open);
    let cleanup = match Arc::try_unwrap(open) {
        Ok(own) => {
            let result = own.begin_shutdown().wait_blocking(Duration::from_secs(30));
            if result.is_ok() {
                eprintln!("NODECRAFT_TIMELINE_STAGING_CLEANUP: owned service cleaned up");
            }
            result.map(|_| ())
        }
        Err(shared) => {
            drop(shared);
            eprintln!("NODECRAFT_TIMELINE_STAGING_CLEANUP: service still shared; normal lifetime retained");
            Ok(())
        }
    };
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup.is_ok(), "the test service did not clean up");
    eprintln!("NODECRAFT_TIMELINE_STAGING_QUOTA: 2 renders retained; third refused");
}
