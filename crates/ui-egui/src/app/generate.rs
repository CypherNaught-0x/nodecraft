//! Generate from a card (#83). Generate quotes a run of exactly that card, shows its price and
//! anything blocking it, and starts it only when the person accepts. `run.start` carries the
//! quote's token and totals, so a canvas or price that changed after the quote is refused by
//! the engine rather than charged. The toolbar over a selected generation card and the image
//! viewer it opens live here too.
use super::*;
use nodecraft_types::ParamControl;
use serde_json::Value as Json;

const MAX_LINES: usize = 8;
const MAX_LINE_CHARS: usize = 240;
const MAX_MODEL_CHARS: usize = 32;

/// What a run from a card does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunKind {
    /// Generate: the card alone.
    Generate,
    /// Refresh a stale result (#211): the card with whatever upstream is stale or has no result.
    Refresh,
}

impl RunKind {
    pub(super) fn mode(self) -> &'static str {
        match self {
            Self::Generate => "selected",
            Self::Refresh => "with_upstream",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Generate => "Generate",
            Self::Refresh => "Refresh",
        }
    }
}

pub(super) struct RunConfirm {
    session: Arc<()>,
    canvas: CanvasId,
    kind: RunKind,
    nodes: Vec<NodeId>,
    pub(super) revision: u64,
    quote: Quote,
    /// Unsaved edits on the card, which the quoted run does not include.
    drafts: bool,
    /// Explicit consent to the unknown part of an incomplete price.
    partial: bool,
    error: Option<String>,
    /// The `run.estimate` request asking its providers to price its by-usage jobs (#663), until
    /// they answer.
    estimating: Option<u64>,
    /// What the providers couldn't price, once they answered.
    estimate_note: Option<String>,
}

#[cfg(test)]
impl RunConfirm {
    /// The nodes this confirmation would run, in order.
    pub(super) fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }

    /// Whether it warns that unsaved edits are not part of the run.
    pub(super) fn drafts(&self) -> bool {
        self.drafts
    }

    /// Whether it waits for its providers' prices.
    pub(super) fn estimating(&self) -> bool {
        self.estimating.is_some()
    }
}

/// What the confirmation shows and `run.start` needs, read from a `run.quote` result.
#[derive(Debug, PartialEq)]
pub(super) struct Quote {
    token: String,
    amounts: serde_json::Map<String, Json>,
    totals: Vec<String>,
    complete: bool,
    allowed: bool,
    reasons: Vec<String>,
    /// Jobs whose price is only known during the run, counted as the engine counts them.
    unpriced_jobs: u64,
    /// Every step is listed, so the count above covers the run.
    listed: bool,
    /// Some batch is priced for every item but may run only those its partial result lacks.
    resumable: bool,
    /// Steps priced by their provider before running (#663).
    quoted: usize,
    /// The planner found nothing in the way; a budget may still refuse the price.
    planned: bool,
}

fn line(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(MAX_LINE_CHARS).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

impl Quote {
    pub(super) fn read(value: &Json) -> Option<Self> {
        let token = value.get("token")?.as_str()?.to_owned();
        let complete = value.get("complete")?.as_bool()?;
        let allowed = value.get("allowed")?.as_bool()?;
        let mut amounts = serde_json::Map::new();
        let mut totals = Vec::new();
        for (code, total) in value.get("totals")?.as_object()? {
            amounts.insert(code.clone(), json!(total.get("micros")?.as_u64()?));
            totals.push(line(total.get("text").and_then(Json::as_str).unwrap_or(code)));
        }
        let strings = |key: &str| value.get(key).and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_str).map(line);
        let reasons = strings("blocked").chain(strings("blocks")).take(MAX_LINES).collect();
        let steps = value.get("steps")?.as_array()?;
        let listed = value.get("steps_total").and_then(Json::as_u64).is_none_or(|total| u64::try_from(steps.len()).is_ok_and(|n| n == total));
        let mut unpriced_jobs = 0u64;
        for step in steps.iter().filter(|step| step.get("unpriced").is_some_and(Json::is_string)) {
            // A step whose job count is unknown counts as one job, as it does for its price.
            unpriced_jobs = unpriced_jobs.checked_add(step.get("jobs").and_then(Json::as_u64).unwrap_or(1))?;
        }
        let resumable = value.get("resumable").and_then(Json::as_array).is_some_and(|nodes| !nodes.is_empty());
        let quoted = value.get("quoted").and_then(Json::as_array).map_or(0, Vec::len);
        let planned = value.get("blocked").and_then(Json::as_array).is_none_or(Vec::is_empty);
        Some(Self { token, amounts, totals, complete, allowed, reasons, unpriced_jobs, listed, resumable, quoted, planned })
    }

    /// Whether the quote can be accepted as shown, given the consent collected so far.
    fn acceptable(&self, partial: bool) -> bool {
        self.allowed && self.listed && (self.complete || partial)
    }

    pub(super) fn price(&self) -> String {
        match (self.totals.is_empty(), self.complete) {
            (true, true) => "No charge".into(),
            (true, false) => "Unknown price".into(),
            (false, true) => self.totals.join(" + "),
            (false, false) => format!("{} so far", self.totals.join(" + ")),
        }
    }
}

impl NodeCraftApp {
    /// Quote a run of one card and ask before starting it. Selection alone never starts a run.
    pub(super) fn request_generate(&mut self, id: NodeId) -> Result<(), Error> {
        self.request_run(id, RunKind::Generate)
    }

    /// Keeps or discards the partial answer a cancelled or failed run left on text card `id` (#504).
    pub(super) fn partial_intent(&mut self, id: NodeId, keep: bool) {
        if let Some(runs) = &mut self.runs {
            let done = if keep { runs.keep_partial(&mut self.session, self.canvas, id) } else { runs.discard_partial(self.canvas, id) };
            self.notice = done.err().map(|error| error.message);
        }
    }

    /// The provider settings runs are set up with: the settings file's, and the defaults in the
    /// browser, which has none (so ComfyUI isn't set up there yet).
    fn provider_settings(&self) -> nodecraft_engine::settings::ProviderSettings {
        #[cfg(not(target_arch = "wasm32"))]
        return self.store.as_ref().map(|store| store.get().providers.clone()).unwrap_or_default();
        #[cfg(target_arch = "wasm32")]
        nodecraft_engine::settings::ProviderSettings::default()
    }

    /// Quote a run from card `id` and ask before starting it.
    pub(super) fn request_run(&mut self, id: NodeId, kind: RunKind) -> Result<(), Error> {
        self.request_runs(vec![id], kind)
    }

    /// Quote one run of the cards `nodes` together and ask before starting it.
    pub(super) fn request_runs(&mut self, nodes: Vec<NodeId>, kind: RunKind) -> Result<(), Error> {
        if nodes.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "nothing to run"));
        }
        if self.run_confirm.is_some() {
            return Ok(());
        }
        if self.runs.is_some() {
            if kind == RunKind::Refresh {
                self.refresh_stale_badges();
            }
            let settings = self.provider_settings();
            let stale = self.stale_badges.as_ref().map(|badges| &badges.nodes).cloned().unwrap_or_default();
            if let Some(runs) = &mut self.runs
                && runs.route(&mut self.session, self.canvas, &nodes, kind, &stale, &settings)? == super::runs::Route::Waiting
            {
                self.notice = Some(super::runs::SETTING_UP.into());
                return Ok(());
            }
        }
        let request = json!({"canvas": self.canvas, "mode": kind.mode(), "nodes": nodes});
        let value = self.session.dispatch("run.quote", &request)?;
        let quote = Quote::read(&value).ok_or_else(|| Error::new(ErrorKind::Internal, "the run's price could not be read; nothing was started"))?;
        // Jobs priced only while they run may have a price their provider can give now (#663),
        // which may also be what a budget needs to allow them; a host that can't ask refuses the
        // command.
        let estimating = (quote.planned && quote.unpriced_jobs > 0)
            .then(|| self.session.dispatch("run.estimate", &request).ok())
            .flatten()
            .and_then(|answer| answer.get("request").and_then(Json::as_u64));
        let drafts = nodes.iter().any(|id| self.editors.get(id).is_some_and(EditorState::has_uncommitted) || self.sheet_draft_pending(*id));
        self.run_confirm = Some(RunConfirm {
            session: self.session.view_identity(),
            canvas: self.canvas,
            kind,
            nodes,
            revision: self.session.revision(),
            quote,
            drafts,
            partial: false,
            error: None,
            estimating,
            estimate_note: None,
        });
        Ok(())
    }

    /// Prices the open confirmation again once its providers answered a `run.estimate`.
    fn estimated(&mut self, estimated: super::runs::Estimated) {
        let request = match &estimated {
            super::runs::Estimated::Answered { request, .. } | super::runs::Estimated::Failed { request, .. } => *request,
        };
        // An answer to another confirmation's request is late; it priced nothing shown here.
        let Some(confirm) = self.run_confirm.as_mut().filter(|confirm| confirm.estimating == Some(request)) else {
            return;
        };
        confirm.estimating = None;
        let unquoted = match estimated {
            super::runs::Estimated::Answered { unquoted, .. } => unquoted,
            super::runs::Estimated::Failed { reason, .. } => {
                confirm.estimate_note = Some(format!("The provider couldn't give a price before running: {}", line(&reason)));
                return;
            }
        };
        if unquoted > 0 {
            confirm.estimate_note = Some(format!("The provider couldn't price {unquoted} job(s) before they run."));
        }
        // A canvas edited since is quoted again by hand, from the stale notice.
        if confirm.revision != self.session.revision() || confirm.error.is_some() {
            return;
        }
        let request = json!({"canvas": confirm.canvas, "mode": confirm.kind.mode(), "nodes": confirm.nodes});
        match self.session.dispatch("run.quote", &request).ok().as_ref().and_then(Quote::read) {
            Some(quote) => {
                confirm.quote = quote;
                // Consent was to the price shown before.
                confirm.partial = false;
            }
            None => confirm.estimate_note = Some("The run couldn't be priced again; quote it again to see the providers' prices.".into()),
        }
    }

    /// Pumps the runs once a frame, and quotes a card whose provider just became ready.
    pub(super) fn tick_runs(&mut self, ctx: &egui::Context) {
        // Not while the recovery dialog decides which document is open: a result recorded now
        // would be lost if the person then recovers the other one.
        #[cfg(not(target_arch = "wasm32"))]
        let deciding = self.recovery.is_some();
        #[cfg(target_arch = "wasm32")]
        let deciding = false;
        let settings = self.runs.as_ref().is_some_and(|runs| runs.has_waiting() && !deciding).then(|| self.provider_settings());
        let Some(runs) = &mut self.runs else {
            return;
        };
        if let Some(settings) = settings {
            runs.resume_waiting(&self.session, &settings);
        }
        let tick = runs.tick(&mut self.session, self.canvas);
        if runs.is_busy() || runs.is_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if (tick.ready.is_some() || tick.abandoned) && self.notice.as_deref() == Some(super::runs::SETTING_UP) {
            self.notice = None;
        }
        if let Some(notice) = tick.notice {
            self.notice = Some(notice);
        }
        for estimated in tick.estimated {
            self.estimated(estimated);
        }
        // A new Soul ID can be picked once the list is fetched again; a fetch already running
        // refuses as busy and the person can refresh later.
        if tick.trained {
            let _ = self.session.dispatch("options.refresh", &serde_json::json!({}));
        }
        // Quoted last: if it can't be, that is the newest thing to say.
        if let Some((nodes, kind)) = tick.ready
            && let Err(error) = self.request_runs(nodes, kind)
        {
            self.notice = Some(error.message);
        }
    }

    /// The open price confirmation. Returns whether it owns this frame's input.
    pub(super) fn run_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut confirm) = self.run_confirm.take() else {
            return false;
        };
        let t = *self.theme.tokens();
        let stale =
            !Arc::ptr_eq(&confirm.session, &self.session.view_identity()) || confirm.canvas != self.canvas || confirm.revision != self.session.revision();
        let mut close = false;
        let mut start = false;
        let mut requote = false;
        let modal = egui::Modal::new(egui::Id::new("run_confirm")).show(ui.ctx(), |ui| {
            ui.set_width(t.dialog_width);
            ui.heading(confirm.kind.title());
            let quote = &confirm.quote;
            if stale {
                ui.label("The canvas changed after this price was quoted. Nothing was started.");
            } else if !quote.allowed {
                ui.label("This run can't start:");
                for reason in &quote.reasons {
                    ui.label(reason);
                }
                if confirm.estimating.is_some() {
                    ui.label(
                        "Asking the provider for a price, which sends it this run's inputs (nothing is charged). The run is checked again when it answers.",
                    );
                }
                if let Some(note) = &confirm.estimate_note {
                    ui.label(note);
                }
            } else {
                ui.label(egui::RichText::new(quote.price()).color(t.text).strong());
                if !quote.complete {
                    if quote.unpriced_jobs > 0 {
                        ui.label(format!("{} job(s) are priced only while they run.", quote.unpriced_jobs));
                    }
                    ui.label("Part of the price is unknown or estimated. The run stops at the amount shown above.");
                    ui.checkbox(&mut confirm.partial, "Accept the unknown part of the price");
                }
                if confirm.estimating.is_some() {
                    ui.label("Asking the provider for a price, which sends it this run's inputs (nothing is charged)…");
                }
                if quote.quoted > 0 {
                    ui.label(format!("{} step(s) are priced by their provider's quote.", quote.quoted));
                }
                if let Some(note) = &confirm.estimate_note {
                    ui.label(note);
                }
                if quote.resumable {
                    ui.label("A batch is priced for every item. Items it finished before aren't charged again if its inputs come out the same.");
                }
                if !quote.listed {
                    ui.label("This run has more steps than can be reviewed here. Run it from the CLI or select fewer nodes.");
                }
                for reason in &quote.reasons {
                    ui.label(reason);
                }
            }
            if confirm.drafts {
                ui.label("Unsaved edits on this card are not part of this run.");
            }
            if let Some(error) = &confirm.error {
                ui.colored_label(t.danger, error);
                // The host may have taken the run before it failed, so the same price is never
                // offered again from here; generating again quotes afresh.
                ui.label("The run may have started. Check its results before generating again.");
            }
            ui.horizontal(|ui| {
                let failed = confirm.error.is_some();
                if stale && !failed {
                    requote = ui.button("Quote again").clicked();
                } else if quote.allowed && !failed {
                    let title = confirm.kind.title();
                    let label = if quote.totals.is_empty() { format!("{title} now") } else { format!("{title} for {}", quote.price()) };
                    start = ui.add_enabled(quote.acceptable(confirm.partial), egui::Button::new(label)).clicked();
                }
                close |= ui.button(if failed { "Close" } else { "Cancel" }).clicked();
            });
        });
        close |= modal.should_close();
        if start && !stale && confirm.error.is_none() {
            let quote = &confirm.quote;
            let mut accept = json!({"token": quote.token, "amounts": quote.amounts});
            if !quote.complete {
                accept["partial"] = json!(true);
            }
            if quote.unpriced_jobs > 0 {
                accept["unpriced_jobs"] = json!(quote.unpriced_jobs);
            }
            match self.session.dispatch("run.start", &json!({"canvas": confirm.canvas, "mode": confirm.kind.mode(), "nodes": confirm.nodes, "accept": accept}))
            {
                Ok(_) => {
                    self.notice = None;
                    close = true;
                }
                Err(error) => confirm.error = Some(error.message),
            }
        }
        if requote {
            if let Err(error) = self.request_runs(confirm.nodes, confirm.kind) {
                self.notice = Some(error.message);
            }
            return true;
        }
        if !close {
            self.run_confirm = Some(confirm);
        }
        true
    }

    /// The toolbar over the one selected generation card: its model and mode, aspect ratio,
    /// strengths, Improve prompt and the image viewer. Edits go through the card intents, so they
    /// share the card's dispatch path.
    pub(super) fn generation_toolbar(&mut self, ui: &mut egui::Ui, viewport: Rect, interactive: bool) -> Vec<(NodeId, Intent)> {
        let mut intents = Vec::new();
        let ids = self.selection.nodes();
        let (Some(&id), 1, true) = (ids.first(), ids.len(), self.selection.edges().is_empty()) else {
            return intents;
        };
        if self.drag.is_some() || self.marquee.is_some() || self.wiring.is_some() {
            return intents;
        }
        let (Some(rect), Some(node), Some(cached)) = (
            self.painted.get(&id).copied(),
            self.session.document().canvases().get(&self.canvas).and_then(|canvas| canvas.nodes().get(&id)),
            self.schemas.get(&id),
        ) else {
            return intents;
        };
        if !card::generate::has_composer(node, &cached.descriptor) {
            return intents;
        }
        let t = self.theme.tokens();
        let screen = self.view.rect_to_screen(viewport.min, rect);
        // Painted includes the cull margin; a card out of view gets no floating toolbar.
        if !screen.intersects(viewport) {
            return intents;
        }
        let above = card::outer_insets(t, self.view.zoom, false).1 * self.view.zoom + t.toolbar_gap + t.toolbar_height;
        let pos = Pos2::new(screen.left(), (screen.top() - above).max(viewport.top()));
        let shown = |name: &str| cached.visible.contains(name);
        let editable = |name: &str| cached.enabled.as_ref().is_none_or(|names| names.contains(name));
        let canvas = self.session.document().canvases().get(&self.canvas);
        let mode = canvas.and_then(|canvas| mode_of(&self.session, canvas, node));
        // A Generate audio card says its prompt as written, so it has nothing to improve (#672).
        let assist = canvas.filter(|_| node.kind != nodecraft_nodes::generation::AUDIO_KIND).map(|canvas| assistable(canvas, node));
        let mut expand = false;
        let mut improve = false;
        // The shown result's media, for Download (#581).
        let shown_result = node.history.iter().find(|generation| Some(generation.id) == node.active);
        let downloads = shown_result.map(|generation| crate::history::export_targets(node, generation)).unwrap_or_default();
        let mut download = None;
        egui::Area::new(egui::Id::new(("generation_toolbar", id))).fixed_pos(pos).order(egui::Order::Middle).show(ui.ctx(), |ui| {
            ui.add_enabled_ui(interactive, |ui| {
                egui::Frame::new()
                    .fill(t.panel_fill)
                    .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                    .corner_radius(t.panel_radius)
                    .inner_margin(t.control_gap)
                    .show(ui, |ui| {
                        ui.set_min_height(t.toolbar_height - 2.0 * t.control_gap);
                        ui.horizontal(|ui| {
                            let model = match node.params.get(nodecraft_nodes::generation::MODEL) {
                                Some(Value::Text(id)) => line(id),
                                _ => "Choose model".into(),
                            };
                            let mut short: String = model.chars().take(MAX_MODEL_CHARS).collect();
                            if model.chars().nth(MAX_MODEL_CHARS).is_some() {
                                short.push('…');
                            }
                            let r = ui
                                .add_enabled(editable(nodecraft_nodes::generation::MODEL), egui::Button::new(&short))
                                .on_hover_text(format!("Model: {model}. Click to change it."));
                            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, r.enabled(), format!("Model {short}")));
                            if r.clicked() {
                                intents.push((id, Intent::Model));
                            }
                            match &mode {
                                Some(Mode::Chosen { label, references }) => {
                                    let hover = if *references {
                                        "The model's mode, chosen by what is wired in: connect reference images to edit from them"
                                    } else {
                                        "The model's mode, chosen by what is wired in"
                                    };
                                    ui.label(egui::RichText::new(label).color(t.text_muted)).on_hover_text(hover);
                                }
                                Some(Mode::NoFit(why)) => {
                                    ui.label(egui::RichText::new("No mode fits").color(t.danger)).on_hover_text(why);
                                }
                                None => {}
                            }
                            if let Some(spec) = cached
                                .roles
                                .aspect
                                .as_deref()
                                .filter(|name| shown(name))
                                .and_then(|name| cached.descriptor.params.params.iter().find(|spec| spec.name == name))
                                && let ParamControl::Choice { options } = &spec.control
                            {
                                let current = match node.params.get(&spec.name).or(spec.default.as_ref()) {
                                    Some(Value::Text(value)) => value.as_str(),
                                    _ => "",
                                };
                                let mut selected = current.to_owned();
                                ui.add_enabled_ui(editable(&spec.name), |ui| {
                                    let label = ui.label(egui::RichText::new(&spec.label).color(t.text_muted));
                                    egui::ComboBox::from_id_salt(("toolbar_aspect", id))
                                        .selected_text(options.iter().find(|o| o.value == current).map_or("Choose…", |o| o.label.as_str()))
                                        .show_ui(ui, |ui| {
                                            for option in options {
                                                ui.selectable_value(&mut selected, option.value.clone(), &option.label);
                                            }
                                        })
                                        .response
                                        .labelled_by(label.id);
                                });
                                if selected != current {
                                    intents.push((id, Intent::Parameter { name: spec.name.clone(), value: Some(Value::Text(selected)) }));
                                }
                            }
                            for name in [&cached.roles.strength, &cached.roles.style_strength].into_iter().flatten().filter(|name| shown(name)) {
                                if let Some(spec) = cached.descriptor.params.params.iter().find(|spec| spec.name == *name)
                                    && let Some(value) = strength(ui, id, node, spec, editable(name), t)
                                {
                                    intents.push((id, Intent::Parameter { name: spec.name.clone(), value: Some(value) }));
                                }
                            }
                            if let Some(assist) = &assist {
                                let r = ui.add_enabled(assist.is_ok(), egui::Button::new("Improve prompt"));
                                improve = match assist {
                                    Ok(()) => {
                                        r.on_hover_text("Add a text card that rewrites this prompt and feeds the rewrite in. Run it to see the rewrite; edit the prompt there from then on").clicked()
                                    }
                                    Err(why) => {
                                        r.on_disabled_hover_text(*why);
                                        false
                                    }
                                };
                            }
                            if nodecraft_engine::split::image_of(node).is_some() {
                                expand = ui.button("Expand").on_hover_text("Show this image larger").clicked();
                            }
                            match downloads.as_slice() {
                                [] => {}
                                [(_, port, item, name)] => {
                                    if ui.button("Download").on_hover_text("Save this result to a file").clicked() {
                                        download = Some(Download::One(port.clone(), *item, name.clone()));
                                    }
                                }
                                many => {
                                    ui.menu_button("Download", |ui| {
                                        for (label, port, item, name) in many {
                                            if ui.button(label).clicked() {
                                                download = Some(Download::One(port.clone(), *item, name.clone()));
                                                ui.close();
                                            }
                                        }
                                        ui.separator();
                                        if ui.button(format!("All {} to a folder…", many.len())).clicked() {
                                            download = Some(Download::All);
                                            ui.close();
                                        }
                                    });
                                }
                            }
                        });
                    });
            });
        });
        if expand && interactive {
            self.viewer = Some(id);
        }
        if let (Some(download), Some(generation), true) = (download, node.active, interactive) {
            match download {
                Download::One(port, item, name) => self.start_export(self.canvas, id, generation, port, item, &name),
                Download::All => {
                    let targets = downloads.into_iter().map(|(_, port, item, name)| (port, item, name)).collect();
                    self.start_export_all(self.canvas, id, generation, targets);
                }
            }
        }
        if improve && interactive {
            self.improve_prompt(id);
        }
        if interactive { intents } else { Vec::new() }
    }

    /// Puts a text card that rewrites `id`'s prompt in front of it (#582), selects it, and reports
    /// a refusal as a notice. Nothing runs until the person runs the new card.
    fn improve_prompt(&mut self, id: NodeId) {
        let document = self.session.document();
        let seed = ("prompt-assist", id, self.session.revision());
        let (Some(node), Some(edge)) = (ids::node_id(document, seed), ids::edge_id(document, seed)) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        match self.session.dispatch("node.prompt_assist", &json!({"canvas": self.canvas, "node": id, "id": node, "edge": edge})) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(node);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// The image viewer the toolbar's Expand opens. It closes when its node or image goes.
    pub(super) fn image_viewer(&mut self, ctx: &egui::Context) {
        let Some(id) = self.viewer else {
            return;
        };
        let t = *self.theme.tokens();
        let Some(asset) = self
            .session
            .document()
            .canvases()
            .get(&self.canvas)
            .and_then(|canvas| canvas.nodes().get(&id))
            .and_then(nodecraft_engine::split::image_of)
            .cloned()
        else {
            self.viewer = None;
            return;
        };
        let edge = t.viewer_width * ctx.pixels_per_point();
        let hash = asset.hash;
        let assets = self.session.assets();
        let preview = self.previews.request_asset(ctx, hash, edge, || assets.preview_read(&asset));
        let mut open = true;
        egui::Window::new("Image").id(egui::Id::new("image_viewer")).open(&mut open).collapsible(false).default_width(t.viewer_width).show(ctx, |ui| {
            let width = ui.available_width();
            let height = match (&preview, asset.meta.width, asset.meta.height) {
                (_, Some(w), Some(h)) if w > 0 && h > 0 => width * h as f32 / w as f32,
                (crate::preview::Preview::Ready { size, .. }, ..) if size[0] > 0 => width * size[1] as f32 / size[0] as f32,
                _ => width,
            };
            let height = if height.is_finite() { height.clamp(1.0, width.max(1.0) * 4.0) } else { width };
            let (area, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
            card::show_image(ui, area, &preview, None, &t);
        });
        if !open {
            self.viewer = None;
        }
    }
}

/// What the toolbar's Download asked to save.
enum Download {
    /// One output (port, item) under its suggested file name.
    One(Option<String>, Option<usize>, String),
    /// Every media output of the shown result, into a folder.
    All,
}

/// A multi-mode model's mode, as the toolbar names it.
enum Mode {
    /// The mode the wired inputs choose, humanised; `references` when some mode reads media.
    Chosen { label: String, references: bool },
    /// No mode takes what is wired, and why.
    NoFit(String),
}

/// Whether Improve prompt can act on `node`, or why not, cheaply enough for every frame; the
/// command checks the rest (the model takes a prompt, a text model is installed) when clicked.
fn assistable(canvas: &Canvas, node: &Node) -> Result<(), &'static str> {
    use nodecraft_nodes::generation::PROMPT;
    if canvas.edges().values().any(|edge| edge.to.node == node.id && edge.to.port == PROMPT) {
        return Err("The prompt comes from a connected card");
    }
    match node.params.get(PROMPT) {
        Some(Value::Text(text)) if text.trim().is_empty() => Err("Write a prompt first"),
        Some(Value::Text(text)) if !nodecraft_engine::assist_cmds::fits(&node.kind, text) => Err("The prompt is too long to improve"),
        Some(Value::Text(_)) => Ok(()),
        _ => Err("Write a prompt first"),
    }
}

/// The mode `node`'s inputs choose, counted as the planner counts them: the items each wired
/// output holds now, or one for an output with no result yet. `None` for a single-mode model, or
/// one the card can't read (the card says why).
fn mode_of(session: &Session, canvas: &Canvas, node: &Node) -> Option<Mode> {
    let contract = session.kinds().get(&node.kind).and_then(|kind| kind.instance_contract(node)).ok()?;
    if contract.modes.len() < 2 {
        return None;
    }
    let counts = admission::wire_counts(canvas, &contract, node);
    let media = |port: &str| node.inputs.iter().any(|input| input.name == port && input.port_type.modality != nodecraft_types::Modality::Text);
    let references = contract.modes.iter().any(|mode| mode.inputs.iter().any(|input| media(&input.port)));
    match contract.resolve_mode(&counts, false) {
        Ok(Some(mode)) => {
            let words = mode.as_str().replace(['-', '_'], " ");
            let mut chars = words.chars();
            let label = chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect());
            Some(Mode::Chosen { label, references })
        }
        Ok(None) => None,
        Err(error) => Some(Mode::NoFit(error.message)),
    }
}

/// A strength slider in the toolbar. A drag is one edit, made when it ends: the value being
/// dragged lives in egui memory until then, with the value it started from, and is dropped if
/// that value changes meanwhile (an undo, an agent) or Escape is pressed. Only a drag or a key
/// on the focused slider sets a value: the slider snapping a stored value to its step never does.
/// Returns the value to set.
fn strength(ui: &mut egui::Ui, id: NodeId, node: &Node, spec: &nodecraft_types::ParamSpec, editable: bool, t: &Tokens) -> Option<Value> {
    let ParamControl::Slider { min: Some(min), max: Some(max), step } = &spec.control else { return None };
    let (low, high) = (min.get(), max.get());
    if !(low.is_finite() && high.is_finite() && low < high) {
        return None;
    }
    let stored = match node.params.get(&spec.name).or(spec.default.as_ref()) {
        Some(Value::Number(n)) => n.get(),
        _ => low,
    };
    let memory = egui::Id::new(("toolbar_strength", id, spec.name.as_str()));
    // (the stored value the drag started from, the dragged value)
    let draft: Option<(f64, f64)> = ui.data(|data| data.get_temp(memory)).filter(|(from, _): &(f64, f64)| *from == stored);
    let mut value = draft.map_or(stored, |(_, value)| value);
    let mut out = None;
    ui.add_enabled_ui(editable, |ui| {
        let label = ui.label(egui::RichText::new(&spec.label).color(t.text_muted));
        let step = step.get();
        let mut slider = egui::Slider::new(&mut value, low..=high).show_value(true).update_while_editing(false);
        if step.is_finite() && step > 0.0 {
            slider = slider.step_by(step);
        }
        let r = ui.add(slider).labelled_by(label.id);
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if r.dragged() && !cancelled {
            ui.data_mut(|data| data.insert_temp(memory, (stored, value)));
            return;
        }
        ui.data_mut(|data| data.remove::<(f64, f64)>(memory));
        let by_user = (r.drag_stopped() && draft.is_some()) || (r.changed() && (r.has_focus() || r.lost_focus()));
        // Snapping to the step moves a value by a rounding error, not a step.
        let moved = (value - stored).abs() > (high - low) * 1e-9;
        if by_user && !cancelled && moved {
            out = nodecraft_types::Number::new(value).ok().map(Value::Number);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(steps: Json, complete: bool) -> Json {
        json!({"token": "t", "complete": complete, "allowed": true, "blocks": [], "blocked": null,
            "totals": {"USD": {"currency": "USD", "micros": 80_000, "text": "$0.08"}}, "steps": steps})
    }

    #[test]
    fn a_quote_counts_unpriced_jobs_as_the_engine_does() {
        let read = Quote::read(&quote(json!([{"jobs": 2, "price": {"micros": 80_000}, "unpriced": null}]), true)).unwrap();
        assert_eq!((read.unpriced_jobs, read.price(), read.acceptable(false)), (0, "$0.08".into(), true));
        assert_eq!(read.amounts["USD"], 80_000);
        let free = Quote::read(&quote(json!([{"jobs": 3, "price": null, "unpriced": null}]), true)).unwrap();
        assert_eq!(free.unpriced_jobs, 0, "a free step is not an unknown price");
        let partial =
            Quote::read(&quote(json!([{"jobs": 3, "price": null, "unpriced": "by usage"}, {"jobs": null, "price": null, "unpriced": "x"}]), false)).unwrap();
        assert_eq!(partial.unpriced_jobs, 4);
        assert!(!partial.acceptable(false), "unknown cost needs its own consent");
        assert!(partial.acceptable(true));
        assert_eq!(partial.price(), "$0.08 so far");
        assert!(!partial.resumable);
        let mut v = quote(json!([]), false);
        v["resumable"] = json!(["00000000-0000-0000-0000-000000000003"]);
        assert!(Quote::read(&v).unwrap().resumable, "a batch that may keep finished items is said so");
    }

    #[test]
    fn an_unreadable_or_unreviewable_quote_is_never_accepted() {
        assert_eq!(Quote::read(&json!({"token": "t"})), None);
        let mut v = quote(json!([]), true);
        v["totals"]["USD"]["micros"] = json!(-1);
        assert_eq!(Quote::read(&v), None, "a malformed amount is not read as zero");
        let mut v = quote(json!([{"jobs": u64::MAX, "unpriced": "x"}, {"jobs": 1, "unpriced": "x"}]), false);
        assert_eq!(Quote::read(&v), None, "an overflowing job count is refused");
        v = quote(json!([]), true);
        v["steps_total"] = json!(300);
        assert!(!Quote::read(&v).unwrap().acceptable(true), "steps beyond the listed ones can't be reviewed");
        v["allowed"] = json!(false);
        v["steps_total"] = Json::Null;
        assert!(!Quote::read(&v).unwrap().acceptable(true));
        let long = "x".repeat(MAX_LINE_CHARS * 2);
        assert_eq!(line(&long).chars().count(), MAX_LINE_CHARS + 1);
    }
}
