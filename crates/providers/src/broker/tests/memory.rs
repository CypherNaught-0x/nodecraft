//! What a hostile plugin service can make the host hold in memory (#174): one job's downloads
//! are capped at 64 MiB, and the jobs of one package share a byte budget, so however many jobs
//! finish at once their downloads are admitted only within it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::executor::block_on;

use super::*;
use crate::broker::ByteBudget;
use crate::broker::adapter::{MAX_JOB_BYTES, MAX_OUTPUT_BYTES, MAX_PACKAGE_BYTES};
use crate::secrets::Secret;
use crate::{BoxFuture, Http, HttpRequest, HttpResponse, Provider, ProviderError};

const PNG_LINK: &str = "https://cdn.acme.example/out/1.png";

/// A service whose jobs are all done, serving each output slowly and noting the most downloads
/// it was serving at once and the largest reply it was allowed.
#[derive(Default)]
struct Finished {
    downloading: Mutex<usize>,
    most: Mutex<usize>,
    largest_allowed: Mutex<usize>,
}

impl Http for Finished {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("the broker never follows redirects")
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        if r.url != PNG_LINK {
            let body = serde_json::to_vec(&json!({"status": "succeeded", "output": {"images": [PNG_LINK]}})).unwrap();
            return Box::pin(async move { Ok(HttpResponse { status: 200, headers: vec![], body }) });
        }
        {
            let mut largest = self.largest_allowed.lock().unwrap();
            *largest = (*largest).max(r.max_response_bytes);
            let mut now = self.downloading.lock().unwrap();
            *now += 1;
            let mut most = self.most.lock().unwrap();
            *most = (*most).max(*now);
        }
        Box::pin(async move {
            std::thread::sleep(Duration::from_millis(60));
            *self.downloading.lock().unwrap() -= 1;
            let png = nodecraft_media::encode_png(2, 2, &[120; 16], &nodecraft_media::Limits::default()).unwrap();
            Ok(HttpResponse { status: 200, headers: vec![], body: png })
        })
    }
}

fn broker(http: Arc<Finished>) -> Broker {
    Broker::new("plugin.acme", template(), Some(Secret::new("broker-fixture-key-not-real").unwrap()), http).unwrap()
}

#[test]
fn a_plugin_jobs_downloads_are_capped_lower_than_a_built_in_adapters() {
    assert_eq!(MAX_OUTPUT_BYTES, 64 * 1024 * 1024);
    assert_eq!(MAX_JOB_BYTES, 64 * 1024 * 1024);
    assert_eq!(MAX_PACKAGE_BYTES, 2 * MAX_JOB_BYTES);
    let http = Arc::new(Finished::default());
    let handle = JobHandle { remote_id: "j-1".into(), data: json!({"model": "acme/v1"}) };
    assert!(matches!(block_on(broker(http.clone()).poll(&handle)).unwrap(), JobProgress::Done(_)));
    assert_eq!(*http.largest_allowed.lock().unwrap(), MAX_OUTPUT_BYTES, "a download may hold at most 64 MiB");
}

/// Three jobs of one package finishing at once, on two of its providers, with room for one
/// job's downloads: they download one job at a time, though each broker's gate allows two.
#[test]
fn a_packages_jobs_share_one_byte_budget() {
    let http = Arc::new(Finished::default());
    let budget = ByteBudget::new(MAX_JOB_BYTES);
    let a = Arc::new(broker(http.clone()).budgeted(Arc::clone(&budget)));
    let b = Arc::new(broker(http.clone()).budgeted(Arc::clone(&budget)));
    assert!(Arc::ptr_eq(a.byte_budget(), b.byte_budget()));
    assert!(!Arc::ptr_eq(a.gate(), b.gate()), "the providers' request gates are their own");
    let jobs: Vec<_> = [&a, &b, &a]
        .into_iter()
        .map(|broker| {
            let broker = Arc::clone(broker);
            std::thread::spawn(move || block_on(broker.poll(&JobHandle { remote_id: "j-1".into(), data: json!({"model": "acme/v1"}) })))
        })
        .collect();
    for job in jobs {
        let JobProgress::Done(done) = job.join().unwrap().unwrap() else { panic!("done") };
        assert!(matches!(done.outputs.as_slice(), [Output::Bytes { .. }]));
    }
    assert_eq!(*http.most.lock().unwrap(), 1, "one job's downloads at a time");
    assert_eq!(budget.held(), 0, "every job gave its room back");
    let small = ByteBudget::new(MAX_JOB_BYTES - 1);
    assert!(!Arc::ptr_eq(broker(http).budgeted(Arc::clone(&small)).byte_budget(), &small), "a budget too small for one job is ignored");
}
