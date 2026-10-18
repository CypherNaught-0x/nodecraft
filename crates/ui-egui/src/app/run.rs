//! Runs from the canvas (#211): stale badges on cards whose result no longer matches its inputs,
//! and the refresh they offer. A refresh quotes *run with upstream* for the node (`run.quote`),
//! asks the user to confirm when it costs money, and starts it (`run.start`) with the quote's
//! token.
//!
//! The app is also the host that runs a started run (the `run.start` service), whoever started
//! it: the badge, the palette, the control channel. Every frame it takes a pending run, prices it
//! again and refuses it if it no longer matches the accepted quote, then runs exactly the plan
//! that was priced with exec's run executor. It records the results with `run.record` (one undo
//! step). Jobs that need no provider run at once, in the frame. Model calls can't run until a
//! provider registry exists (#201), so a refresh that needs one says so instead of asking for
//! money.
use super::*;
use nodecraft_catalog::Catalog;
use nodecraft_engine::run_cmds::{self, BudgetState, MAX_RESULTS, RUN_SERVICE, RunCatalog};
use nodecraft_exec::cost::{Budgets, Spent};
use nodecraft_exec::run::{Progress, Run, RunLimits, RunOptions};
use nodecraft_exec::scheduler::RetryPolicy;
use nodecraft_nodes::JobSpec;
use serde_json::Value as Json;

/// A refresh waiting for the user to confirm its price.
pub struct Confirm {
    pub node: NodeId,
    pub request: Json,
    pub token: String,
    pub amounts: serde_json::Map<String, Json>,
    pub partial: bool,
    /// The price, as shown ("0.12 USD", or a note that part of it isn't known).
    pub price: String,
    /// The document revision it was priced at: an edit closes the confirmation.
    revision: u64,
}

/// The stale set, and the (canvas, revision) it was computed for.
type StaleCache = ((CanvasId, u64), Rc<BTreeSet<NodeId>>);

/// Most failure reasons a run's notice lists.
const MAX_REASONS: usize = 3;

/// The app's run state: the catalog and caps runs are priced against, the stale set (kept per
/// canvas and revision), a pending confirmation, and runs the `run.start` service handed over.
pub struct RunHost {
    pub confirm: Option<Confirm>,
    stale: Option<StaleCache>,
    mailbox: Rc<RefCell<Option<Json>>>,
    runs: u64,
    /// Seeds run ids with randomness where the platform has it (so a re-run after a restart
    /// doesn't reuse an earlier run's ids).
    seed: std::collections::hash_map::RandomState,
}

impl RunHost {
    /// Registers the `run.start` service and the pricing resources on `session`.
    pub(super) fn install(session: &mut Session) -> Result<Self, Error> {
        session.swap_resource(Some(RunCatalog::new(Catalog::builtin()?)?));
        session.swap_resource(Some(BudgetState { budgets: Budgets::default(), spent: Spent::default() }));
        let mailbox: Rc<RefCell<Option<Json>>> = Rc::default();
        let slot = Rc::clone(&mailbox);
        session.register_host_service(RUN_SERVICE, move |request| {
            let mut pending = slot.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "the run host is busy"))?;
            if pending.is_some() {
                return Err(Error::new(ErrorKind::Conflict, "a run is already starting"));
            }
            *pending = Some(request.clone());
            Ok(json!({"accepted": true}))
        })?;
        let seed = std::collections::hash_map::RandomState::new();
        Ok(Self { confirm: None, stale: None, mailbox, runs: 0, seed })
    }
}

/// A short notice from failure reasons: distinct ones, at most a few.
fn failures_notice(failures: Vec<String>) -> Option<String> {
    let mut distinct: Vec<String> = Vec::new();
    for reason in failures {
        if !distinct.contains(&reason) {
            distinct.push(reason);
        }
    }
    if distinct.is_empty() {
        return None;
    }
    let more = distinct.len().saturating_sub(MAX_REASONS);
    let mut text = format!("Some steps didn't run: {}", distinct.iter().take(MAX_REASONS).cloned().collect::<Vec<_>>().join("; "));
    if more > 0 {
        text.push_str(&format!(" (and {more} more)"));
    }
    Some(text)
}

/// The friendlier notice for a run that changed since its quote.
fn changed(error: Error) -> String {
    if error.message.contains("quote it again") { "The canvas changed; refresh again.".into() } else { error.message }
}

impl NodeCraftApp {
    /// Prices runs against `catalog` from now on.
    pub fn set_run_catalog(&mut self, catalog: Catalog) -> Result<(), Error> {
        self.session.swap_resource(Some(RunCatalog::new(catalog)?));
        Ok(())
    }

    /// Nodes whose result is stale, recomputed only when the canvas or document changed. An
    /// error is reported once, and counts as nothing stale.
    pub(super) fn stale_nodes(&mut self) -> Rc<BTreeSet<NodeId>> {
        let key = (self.canvas, self.session.revision());
        if let Some((at, nodes)) = &self.run.stale
            && *at == key
        {
            return Rc::clone(nodes);
        }
        let nodes = match self.session.dispatch("canvas.stale", &json!({"canvas": self.canvas})) {
            Ok(result) => result.get("nodes").and_then(|n| serde_json::from_value::<BTreeSet<NodeId>>(n.clone()).ok()).unwrap_or_default(),
            Err(error) => {
                self.notice = Some(format!("Can't check for stale results: {}", error.message));
                BTreeSet::new()
            }
        };
        let nodes = Rc::new(nodes);
        self.run.stale = Some((key, Rc::clone(&nodes)));
        nodes
    }

    /// Refreshes `node` with what it needs upstream: runs at once when free, else asks first. A
    /// UI action (it may open the confirmation); programmatic callers use `run.quote` and
    /// `run.start`, which never open dialogs.
    pub(crate) fn refresh(&mut self, node: NodeId) {
        let request = json!({"canvas": self.canvas, "mode": "with_upstream", "nodes": [node]});
        let quote = match self.session.dispatch("run.quote", &request) {
            Ok(quote) => quote,
            Err(error) => {
                self.notice = Some(error.message);
                return;
            }
        };
        if quote.get("allowed") != Some(&Json::Bool(true)) {
            let reasons: Vec<String> = ["blocked", "blocks"]
                .iter()
                .filter_map(|k| quote.get(*k).and_then(Json::as_array))
                .flatten()
                .filter_map(|r| r.as_str().map(str::to_owned))
                .collect();
            self.notice = Some(if reasons.is_empty() { "Can't refresh this node right now.".into() } else { format!("Can't refresh: {}", reasons.join(" ")) });
            return;
        }
        // Don't ask for money for a run that can't happen yet.
        if let Ok(priced) = run_cmds::price(&self.session, &request)
            && let Some(provider) = priced.plan.iter().flat_map(|p| p.steps.iter()).flat_map(|s| s.jobs.iter().flatten()).find_map(|job| match job {
                JobSpec::Generate { provider, .. } => Some(provider.clone()),
                JobSpec::Source { .. } => None,
            })
        {
            self.notice = Some(format!("This refresh calls a model, and no {provider} provider is set up yet."));
            return;
        }
        let token = quote.get("token").and_then(Json::as_str).unwrap_or_default().to_owned();
        let complete = quote.get("complete") == Some(&Json::Bool(true));
        let totals = quote.get("totals").and_then(Json::as_object).cloned().unwrap_or_default();
        let amounts: serde_json::Map<String, Json> =
            totals.iter().map(|(currency, money)| (currency.clone(), money.get("micros").cloned().unwrap_or(Json::Null))).collect();
        let free = totals.values().all(|m| m.get("micros").and_then(Json::as_u64) == Some(0));
        if free && complete {
            self.start_run(request, token, amounts, false);
            return;
        }
        let mut price: Vec<String> = totals.values().filter_map(|m| m.get("text").and_then(Json::as_str).map(str::to_owned)).collect();
        if price.is_empty() {
            price.push("nothing known".into());
        }
        let mut text = price.join(" + ");
        if !complete {
            text.push_str(" (part of the price isn't known)");
        }
        self.run.confirm = Some(Confirm { node, request, token, amounts, partial: !complete, price: text, revision: self.session.revision() });
    }

    fn start_run(&mut self, mut request: Json, token: String, amounts: serde_json::Map<String, Json>, partial: bool) {
        if let Some(fields) = request.as_object_mut() {
            fields.insert("accept".into(), json!({"token": token, "amounts": amounts, "partial": partial}));
        }
        match self.session.dispatch("run.start", &request) {
            Ok(_) => self.host_pending_run(),
            Err(error) => self.notice = Some(changed(error)),
        }
    }

    /// The confirmation for a paid refresh, when one is pending. An edit closes it (the price it
    /// shows no longer holds).
    pub(super) fn show_confirm(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.run.confirm else { return };
        if confirm.revision != self.session.revision() {
            self.run.confirm = None;
            self.notice = Some("The canvas changed; refresh again.".into());
            return;
        }
        let (mut run, mut cancel) = (false, false);
        egui::Window::new("Refresh").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(format!("This refresh costs {}.", confirm.price));
            ui.horizontal(|ui| {
                run = ui.button("Run").clicked();
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if cancel {
            self.run.confirm = None;
        } else if run && let Some(confirm) = self.run.confirm.take() {
            self.start_run(confirm.request, confirm.token, confirm.amounts, confirm.partial);
        }
    }

    /// Runs what the `run.start` service handed over, if anything (called every frame, so runs
    /// started from anywhere run).
    pub(super) fn host_pending_run(&mut self) {
        let Some(request) = self.run.mailbox.try_borrow_mut().ok().and_then(|mut slot| slot.take()) else { return };
        match self.execute(&request) {
            Ok(message) => self.notice = message,
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// A run id unique to this document state and session: it covers every generation id on the
    /// canvas, a counter and per-process randomness (where the platform has it).
    fn run_id(&mut self) -> [u8; 16] {
        use std::hash::{BuildHasher, Hash, Hasher};
        self.run.runs = self.run.runs.saturating_add(1);
        let mut hasher = self.run.seed.build_hasher();
        (self.session.document().id(), self.canvas, self.session.revision(), self.run.runs).hash(&mut hasher);
        let low = hasher.finish();
        if let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
            for node in canvas.nodes().values() {
                for generation in &node.history {
                    generation.id.hash(&mut hasher);
                }
            }
        }
        let high = hasher.finish();
        let mut id = [0u8; 16];
        for (slot, byte) in id.iter_mut().zip(low.to_le_bytes().into_iter().chain(high.to_le_bytes())) {
            *slot = byte;
        }
        id
    }

    /// Runs a started run: checks it is still what was quoted and allowed, runs exactly the plan
    /// that was priced (on the canvas it was priced for), and records the results.
    /// `Ok(Some(text))` reports steps that didn't produce a result.
    pub(super) fn execute(&mut self, request: &Json) -> Result<Option<String>, Error> {
        let run_request = request.get("run").cloned().unwrap_or(Json::Null);
        let priced = run_cmds::price(&self.session, &run_request)?;
        if request.get("token").and_then(Json::as_str) != Some(priced.token.as_str()) {
            return Err(Error::new(ErrorKind::Conflict, "The canvas changed before the run started; refresh again."));
        }
        if !priced.allowed {
            return Err(Error::new(ErrorKind::InvalidInput, "The budgets no longer allow this run."));
        }
        let run_plan = priced.plan.ok_or_else(|| Error::new(ErrorKind::InvalidInput, "The run can't start any more; refresh again."))?;
        // Results are recorded in one step, which takes at most this many.
        if run_plan.steps.len() > MAX_RESULTS {
            return Err(Error::new(ErrorKind::InvalidInput, format!("This run has {} steps; run at most {MAX_RESULTS} at a time.", run_plan.steps.len())));
        }
        let canvas_id = priced.canvas;
        let run_id = self.run_id();
        let document = self.session.document();
        let canvas = document.canvases().get(&canvas_id).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
        let kinds = self.session.kinds();
        let options = RunOptions { id: run_id, first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
        let (mut run, first) = Run::start(&run_plan, canvas_id, canvas, kinds, options)?;
        let mut results = Vec::new();
        let mut failures = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        let mut absorb = |progress: Progress, queue: &mut std::collections::VecDeque<nodecraft_exec::scheduler::JobSpec>| {
            queue.extend(progress.jobs);
            results.extend(progress.finished.into_iter().map(|r| json!({"node": r.node, "generation": r.generation})));
            failures.extend(progress.failed.into_iter().map(|f| f.reason));
        };
        absorb(first, &mut queue);
        while let Some(job) = queue.pop_front() {
            let progress = match run.work(job.id).cloned() {
                Some(JobSpec::Source { output, value, .. }) => run.job_done(kinds, job.id, vec![(output, value)]),
                Some(JobSpec::Generate { provider, .. }) => run.job_failed(job.id, &format!("no {provider} provider is set up yet")),
                None => run.job_failed(job.id, "the job has no work"),
            };
            absorb(progress, &mut queue);
        }
        if !results.is_empty() {
            self.session.dispatch("run.record", &json!({"canvas": canvas_id, "results": results}))?;
        }
        Ok(failures_notice(failures))
    }
}
