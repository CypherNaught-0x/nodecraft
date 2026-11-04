//! A plugin template's `limits.concurrency` through a real run (#174): a batch of N + 1 prompts
//! into one card on a plugin provider allowing N never has more than N of its jobs at the
//! service at once. A fake service only; no live calls.
use super::*;
use crate::commands::Services;
use crate::jobs::JobBudget;
use crate::run_host::RunHost;
use nodecraft_exec::cost::{Budgets, UsageLog};
use nodecraft_exec::driver::{Runtime, TaskFuture};
use nodecraft_exec::scheduler::RetryPolicy;
use nodecraft_types::EdgeId;
use std::time::{Duration, Instant};

const BATCH: NodeId = NodeId::from_bytes([11; 16]);
/// Polls a job takes to finish, so its jobs overlap at the service when nothing caps them.
const POLLS: usize = 2;
const PNG_ORIGIN: &str = "https://cdn.acme.example";

/// Each task on its own thread; sleeps on a timer thread.
struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The plugin's service: queues each job and finishes it after [`POLLS`] polls, noting the most
/// jobs it held at once.
#[derive(Default)]
struct Service {
    /// Every submit's reply is lost on the way back, after the service took the job.
    lose_replies: bool,
    jobs: Mutex<BTreeMap<String, usize>>,
    submitted: Mutex<usize>,
    most: Mutex<usize>,
}

fn answer(status: u16, body: Vec<u8>) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status, headers: vec![], body })
}

impl nodecraft_providers::http::Http for Service {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("the broker never follows redirects")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let json = |v: serde_json::Value| serde_json::to_vec(&v).unwrap();
        let reply = if request.url == format!("{ORIGIN}/v1/jobs") {
            let mut jobs = self.jobs.lock().unwrap();
            let mut submitted = self.submitted.lock().unwrap();
            *submitted += 1;
            let id = format!("j-{submitted}");
            jobs.insert(id.clone(), 0);
            let mut most = self.most.lock().unwrap();
            *most = (*most).max(jobs.len());
            if self.lose_replies {
                Err(ProviderError::new(nodecraft_providers::ErrorKind::Timeout, "the reply was lost"))
            } else {
                answer(200, json(json!({"id": id})))
            }
        } else if let Some(id) = request.url.strip_prefix(&format!("{ORIGIN}/v1/jobs/")) {
            let mut jobs = self.jobs.lock().unwrap();
            match jobs.get_mut(id) {
                None => answer(404, json(json!({"error": "no such job"}))),
                Some(polls) if *polls + 1 >= POLLS => {
                    jobs.remove(id);
                    answer(200, json(json!({"status": "succeeded", "output": {"images": [format!("{PNG_ORIGIN}/out/{id}.png")]}})))
                }
                Some(polls) => {
                    *polls += 1;
                    answer(200, json(json!({"status": "running"})))
                }
            }
        } else if request.url.starts_with(PNG_ORIGIN) {
            let pixels: Vec<u8> = (0..16u32).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
            answer(200, nodecraft_media::encode_png(4, 4, &pixels, &nodecraft_media::Limits::default()).unwrap())
        } else {
            answer(404, Vec::new())
        };
        Box::pin(async move { reply })
    }
}

/// `limit` requests at once, polled as fast as a template may.
fn limited(limit: u32) -> String {
    let mut t: serde_json::Value = serde_json::from_str(&priced("40000")).unwrap();
    t["limits"]["concurrency"] = json!(limit);
    t["poll"]["interval_ms"] = json!(nodecraft_providers::broker::template::MIN_POLL_MS);
    t.to_string()
}

/// Runs a batch of `prompts` into one card on the plugin provider allowing `limit` at once, and
/// returns the service.
fn run(limit: u32, prompts: usize) -> Arc<Service> {
    let (service, _) = run_on(Service::default(), limit, prompts, RetryPolicy { max_attempts: 1, ..Default::default() }, None);
    service
}

/// [`run`] against `service`, retrying as `retry` says; the service and the run's jobs.
/// With `journal`, the run journal is kept there and every saved job's plugin grant is noted in
/// `saved`.
fn run_on(
    service: Service,
    limit: u32,
    prompts: usize,
    retry: RetryPolicy,
    journal: Option<(&crate::run_journal::MemoryJournal, &mut Vec<Option<crate::run_journal::PluginGrant>>)>,
) -> (Arc<Service>, Vec<crate::run_jobs::JobView>) {
    let mut journal = journal;
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "plugin-concurrency".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(retry);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap().with_services(services);
    s.set_plugins(PluginSnapshot::of(&granted(&limited(limit), &[ORIGIN, PNG_ORIGIN]))).unwrap();
    let mut host = host;
    host.install(&mut s);
    if let Some((store, _)) = &journal {
        host.open_journal(Box::new((*store).clone())).unwrap();
    }

    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": CARD, "kind": nodecraft_nodes::generation::KIND, "pos": {"x": 0, "y": 0}})).unwrap();
    let preview = s.dispatch("node.model_preview", &json!({"canvas": CANVAS, "nodes": [CARD], "model": MODEL})).unwrap();
    let commit = json!({"canvas": CANVAS, "nodes": [CARD], "model": MODEL, "expected_revision": s.revision(), "preview_binding": preview["binding"]});
    s.dispatch("node.set_model", &commit).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": BATCH, "kind": nodecraft_nodes::batch::ID, "pos": {"x": -300, "y": 0}})).unwrap();
    let text: Vec<String> = (0..prompts).map(|n| format!("prompt {n}")).collect();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BATCH, "name": "text", "value": Value::Text(text.join("\n"))})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([12; 16]), "from": {"node": BATCH, "port": "item"}, "to": {"node": CARD, "port": "prompt"}, "order": 0});
    s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();

    let keys = MemoryStore::default();
    keys.set(&Account::new("plugin.io.example.acme:api.acme.example").unwrap(), &Secret::new("plugin-fixture-key-not-real").unwrap()).unwrap();
    let lost = service.lose_replies;
    let service = Arc::new(service);
    let provider = Arc::clone(s.plugins().provider(PROVIDER).unwrap());
    let mut setup = crate::provider_setup::ProviderSetup::default();
    let http: Arc<dyn nodecraft_providers::http::Http> = service.clone();
    assert_eq!(setup.ensure(PROVIDER, |a| broker(a, &provider, &keys, http.clone(), GrantCheck::this_window())), &Readiness::Ready);
    let routing = setup.routing([(CARD, PROVIDER)]).unwrap();
    s.set_provider_routing(routing).unwrap();

    let request = json!({"canvas": CANVAS, "mode": "selected", "nodes": [CARD]});
    let q = s.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let amounts: serde_json::Map<_, _> = q["totals"].as_object().unwrap().iter().map(|(currency, total)| (currency.clone(), total["micros"].clone())).collect();
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != true, "unpriced_jobs": 0, "amounts": amounts});
    s.dispatch("run.start", &start).unwrap();
    let mut notices = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        notices.extend(host.pump(&mut s));
        if let Some((store, saved)) = &mut journal {
            saved.extend(store.saved().into_iter().flat_map(|j| j.submitted).map(|entry| entry.plugin));
        }
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(provider.gate.held(), 0, "every request slot was given back");
    let card = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&CARD).unwrap();
    let finished = card.history.iter().any(|g| Some(g.id) == card.active && g.incomplete.is_none());
    assert!(finished != lost, "the batch finished only if its replies came back: {notices:?}");
    (service, host.jobs())
}

#[test]
fn n_plus_one_jobs_never_exceed_the_templates_limit_at_the_service() {
    let service = run(2, 3);
    assert_eq!(*service.submitted.lock().unwrap(), 3, "every prompt was sent once");
    assert_eq!(*service.most.lock().unwrap(), 2, "the service held two jobs at once and never three");
}

#[test]
fn a_template_allowing_one_runs_its_jobs_one_at_a_time() {
    let service = run(1, 2);
    assert_eq!(*service.submitted.lock().unwrap(), 2);
    assert_eq!(*service.most.lock().unwrap(), 1);
}

/// #174: a submit whose reply is lost may have created a billed job. The template names an
/// idempotency header, but that is only its publisher's claim, so the host never sends the job
/// again on its word, even with retries allowed: it is sent once and ends as possibly charged.
#[test]
fn a_lost_submit_reply_is_sent_once_and_ends_as_possibly_charged() {
    let retry = RetryPolicy { max_attempts: 3, base_delay_ms: 1, max_delay_ms: 1 };
    let (service, jobs) = run_on(Service { lose_replies: true, ..Service::default() }, 2, 1, retry, None);
    assert_eq!(*service.submitted.lock().unwrap(), 1, "sent once, never again under the claimed key");
    let [job] = jobs.as_slice() else { panic!("{jobs:?}") };
    assert!(job.may_have_been_charged(), "{job:?}");
}

/// #174: a plugin job is journaled with the grant it was sent under, so a restart follows it only
/// under that grant.
#[test]
fn a_plugin_job_is_journaled_with_the_grant_it_was_sent_under() {
    let store = crate::run_journal::MemoryJournal::default();
    let mut saved = Vec::new();
    run_on(Service::default(), 2, 2, RetryPolicy { max_attempts: 1, ..Default::default() }, Some((&store, &mut saved)));
    let expected = PluginSnapshot::of(&granted(&limited(2), &[ORIGIN, PNG_ORIGIN])).provider(PROVIDER).map(|p| crate::run_journal::PluginGrant::of(&p.stamp));
    assert!(expected.is_some());
    assert!(!saved.is_empty(), "the running jobs were journaled");
    assert!(saved.iter().all(|grant| *grant == expected), "{saved:?}");
}
