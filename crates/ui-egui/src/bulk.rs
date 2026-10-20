//! Bulk view state. Mixed is presentation state, never a document Value or a schema default.
use nodecraft_engine::Session;
use nodecraft_nodes::NodeDescriptor;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SharedValue {
    /// Stored absence remains distinct from an explicit value equal to the default.
    Uniform(Option<Value>),
    Mixed,
}

pub(crate) enum Intent {
    Parameters(serde_json::Value),
    OpenModel { canvas: CanvasId, exact_ids: Vec<NodeId>, revision: u64 },
}

pub(crate) struct Snapshot {
    identity: Arc<()>,
    catalog: Option<nodecraft_engine::generation_catalog::GenerationCatalogStamp>,
    pub revision: u64,
    pub canvas: CanvasId,
    pub ids: BTreeSet<NodeId>,
    pub descriptor: NodeDescriptor,
    pub fields: BTreeMap<String, SharedValue>,
    pub enabled: BTreeSet<String>,
    defaults_mixed: BTreeSet<String>,
    model_repair_only: bool,
}
impl Snapshot {
    pub fn matches(&self, session: &Session, canvas: CanvasId, ids: &BTreeSet<NodeId>) -> bool {
        self.canvas == canvas
            && self.revision == session.revision()
            && self.ids == *ids
            && Arc::ptr_eq(&self.identity, &session.view_identity())
            && self.catalog.as_ref().is_none_or(|stamp| session.generation_catalog_stamp().ok().as_ref() == Some(stamp))
    }
    pub fn read(session: &Session, canvas: CanvasId, ids: &BTreeSet<NodeId>) -> Result<Self, Error> {
        if !(2..=nodecraft_engine::bulk_cmds::MAX_TARGETS).contains(&ids.len()) {
            return Err(invalid("Select between 2 and 256 nodes to edit shared parameters."));
        }
        let graph = session.document().canvases().get(&canvas).ok_or_else(|| invalid("The selected canvas is unavailable."))?;
        let selected = ids.iter().map(|id| graph.nodes().get(id).ok_or_else(|| invalid("A selected node is unavailable."))).collect::<Result<Vec<_>, _>>()?;
        nodecraft_engine::bulk_cmds::check_selection_payload(selected.iter().copied())?;
        let catalog =
            if selected.iter().any(|node| nodecraft_nodes::generation::is_generation(&node.kind)) { Some(session.generation_catalog_stamp()?) } else { None };
        let mut descriptors = Vec::with_capacity(ids.len());
        let mut contracts = Vec::with_capacity(ids.len());
        let mut nodes = Vec::with_capacity(ids.len());
        let mut resolutions = Vec::with_capacity(ids.len());
        let mut schema_bytes = 0usize;
        let mut model_repair_only = false;
        for node in selected {
            let kind = session.kinds().get(&node.kind)?;
            if nodecraft_nodes::generation::is_generation(&node.kind) {
                session.generation_catalog()?.validate_instance_binding(kind, node)?;
            }
            // Resolve first: future versions and invalid payloads remain refused. A saved
            // unavailable generation model can retain its former ports; only the common
            // explicit model repair action may operate on that opaque source layout.
            let instance = kind.resolve_instance(node)?;
            let unavailable = if nodecraft_nodes::generation::is_generation(&node.kind) {
                match node.params.get(nodecraft_nodes::generation::MODEL) {
                    Some(value @ Value::Text(model)) if session.generation_catalog()?.model(&node.kind, model).is_none() => {
                        nodecraft_types::ParamControl::Model.validate_value(value)?;
                        true
                    }
                    None => true,
                    _ => false,
                }
            } else {
                false
            };
            model_repair_only |= unavailable;
            if !unavailable && (node.inputs != instance.inputs || node.outputs != instance.outputs) {
                return Err(invalid("A selected node has incompatible ports."));
            }
            if nodes.first().is_some_and(|first: &&nodecraft_graph::Node| first.kind != node.kind) {
                return Err(invalid("Select nodes of the same kind."));
            }
            let descriptor_bytes = crate::app::descriptor_bytes(&instance).ok_or_else(|| invalid("The shared parameter schema exceeds the view budget."))?;
            schema_bytes = schema_bytes.checked_add(descriptor_bytes).ok_or_else(|| invalid("The selection schema exceeds the view budget."))?;
            let contract = kind.instance_contract(node)?;
            account_contract(&contract, &mut schema_bytes)?;
            resolutions.push(contract.resolve(&node.params, false)?);
            contracts.push(contract);
            descriptors.push(instance);
            nodes.push(node);
        }
        let mut descriptor = descriptors.first().ok_or_else(|| invalid("No shared parameter schema is available."))?.as_ref().clone();
        let refs: Vec<_> = contracts.iter().collect();
        descriptor.params.params = nodecraft_engine::bulk_cmds::shared_node_specs(&refs);
        if model_repair_only {
            descriptor.params.params.retain(|spec| spec.name == nodecraft_nodes::generation::MODEL && spec.control == nodecraft_types::ParamControl::Model);
        }
        let mut fields = BTreeMap::new();
        let mut enabled = BTreeSet::new();
        let mut defaults_mixed = BTreeSet::new();
        for spec in &descriptor.params.params {
            if !resolutions.iter().all(|resolution| resolution.get(&spec.name).is_some_and(|state| state.visible)) {
                continue;
            }
            if resolutions.iter().all(|resolution| resolution.get(&spec.name).is_some_and(|state| state.enabled)) {
                enabled.insert(spec.name.clone());
            }
            let shared = match nodecraft_types::shared_value(nodes.iter().map(|node| node.params.get(&spec.name))) {
                nodecraft_types::Shared::Same(value) => SharedValue::Uniform(Some(value)),
                nodecraft_types::Shared::Unset => SharedValue::Uniform(None),
                nodecraft_types::Shared::Mixed => SharedValue::Mixed,
            };
            if matches!(shared, SharedValue::Uniform(None))
                && resolutions.first().is_some_and(|first| {
                    resolutions.iter().skip(1).any(|other| first.get(&spec.name).map(|state| &state.value) != other.get(&spec.name).map(|state| &state.value))
                })
            {
                defaults_mixed.insert(spec.name.clone());
            }
            fields.insert(spec.name.clone(), shared);
        }
        Ok(Self {
            identity: session.view_identity(),
            catalog,
            revision: session.revision(),
            canvas,
            ids: ids.clone(),
            descriptor,
            fields,
            enabled,
            defaults_mixed,
            model_repair_only,
        })
    }
}
// Account the complete contract before retaining per-target resolutions/default clones.
fn account_contract(contract: &nodecraft_nodes::NodeContract, bytes: &mut usize) -> Result<(), Error> {
    struct Counter<'a>(&'a mut usize);
    impl std::io::Write for Counter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            *self.0 = self.0.saturating_add(bytes.len().saturating_mul(4));
            if *self.0 > crate::app::MAX_SCHEMA_CACHE_BYTES {
                return Err(std::io::Error::other("bulk contract view budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(bytes), contract).map_err(|_| invalid("The selection contracts exceed the view budget."))
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

struct Refusal {
    identity: Arc<()>,
    revision: u64,
    canvas: CanvasId,
    ids: BTreeSet<NodeId>,
    catalog_dependent: bool,
    catalog: Option<nodecraft_engine::generation_catalog::GenerationCatalogStamp>,
}
impl Refusal {
    fn token(session: &Session, canvas: CanvasId, ids: &BTreeSet<NodeId>) -> (bool, Option<nodecraft_engine::generation_catalog::GenerationCatalogStamp>) {
        let needs = session
            .document()
            .canvases()
            .get(&canvas)
            .is_some_and(|graph| ids.iter().any(|id| graph.nodes().get(id).is_some_and(|node| nodecraft_nodes::generation::is_generation(&node.kind))));
        (needs, if needs { session.generation_catalog_stamp().ok() } else { None })
    }
    fn new(session: &Session, canvas: CanvasId, ids: &BTreeSet<NodeId>) -> Self {
        let (catalog_dependent, catalog) = Self::token(session, canvas, ids);
        Self { identity: session.view_identity(), revision: session.revision(), canvas, ids: ids.clone(), catalog_dependent, catalog }
    }
    fn matches(&self, session: &Session, canvas: CanvasId, ids: &BTreeSet<NodeId>) -> bool {
        self.revision == session.revision()
            && self.canvas == canvas
            && self.ids == *ids
            && Arc::ptr_eq(&self.identity, &session.view_identity())
            && Self::token(session, canvas, ids) == (self.catalog_dependent, self.catalog.clone())
    }
}

#[derive(Default)]
pub(crate) struct Panel {
    snapshot: Option<Snapshot>,
    refused: Option<Refusal>,
    editor: crate::card::EditorState,
    editing_mixed: BTreeSet<String>,
    error: Option<String>,
}
impl Panel {
    pub fn show(
        &mut self,
        ui: &egui::Ui,
        session: &Session,
        canvas: CanvasId,
        ids: &BTreeSet<NodeId>,
        enabled: bool,
        t: &crate::theme::Tokens,
    ) -> (bool, Option<Intent>) {
        if ids.len() < 2 {
            *self = Self::default();
            return (false, None);
        }
        let current = self.snapshot.as_ref().is_some_and(|snapshot| snapshot.matches(session, canvas, ids))
            || self.refused.as_ref().is_some_and(|refused| refused.matches(session, canvas, ids));
        if !current {
            // A draft can never follow a selection or document replacement into other nodes.
            self.editor = Default::default();
            self.editor.set_draft_limit(16 * 1024 * 1024);
            self.editing_mixed.clear();
            match Snapshot::read(session, canvas, ids) {
                Ok(snapshot) => {
                    self.snapshot = Some(snapshot);
                    self.refused = None;
                    self.error = None;
                }
                Err(error) => {
                    self.snapshot = None;
                    self.refused = Some(Refusal::new(session, canvas, ids));
                    self.error = Some(error.message);
                }
            }
        }
        // Freeze ownership before controls process the click/blur. Otherwise a later control
        // could replace this frame's text commit and discard the user's draft on refresh.
        let text_owner = ui.ctx().memory(|memory| memory.focused()).filter(|id| egui::TextEdit::load_state(ui.ctx(), *id).is_some());
        let viewport = ui.max_rect();
        let mut request = None;
        let shown = egui::Window::new("Shared parameters")
            .id(ui.id().with("bulk_parameters"))
            .anchor(egui::Align2::RIGHT_TOP, [-t.navigation_margin, t.control_row_height + t.navigation_margin])
            .default_width(t.card_width)
            .resizable(false).collapsible(false).movable(false)
            .frame(egui::Frame::window(ui.style()).fill(t.panel_fill).stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke)).corner_radius(t.panel_radius))
            .show(ui.ctx(), |ui| {
                ui.label(format!("{} selected nodes", ids.len()));
                if let Some(snapshot) = &self.snapshot {
                    ui.label(&snapshot.descriptor.label);
                    if snapshot.model_repair_only {
                        ui.label("Some saved models are unavailable. Only shared model repair is offered.");
                    }
                    if snapshot.fields.is_empty() { ui.label("No parameters are shared by this selection."); }
                    egui::ScrollArea::vertical().max_height((viewport.height() - t.minimap_height - t.navigation_toolbar_height - t.control_row_height * 4.0).max(t.control_row_height))
                        .show(ui, |ui| {
                            ui.add_enabled_ui(enabled, |ui| {
                                for spec in &snapshot.descriptor.params.params {
                                    let Some(shared) = snapshot.fields.get(&spec.name) else { continue };
                                    let row_enabled = snapshot.enabled.contains(&spec.name) && text_owner.is_none_or(|id| self.editor.field_owns_focus(&spec.name, id));
                                    ui.add_enabled_ui(row_enabled, |ui| { ui.push_id(&spec.name, |ui| {
                                        let label = ui.label(&spec.label);
                                        if let Some(description) = &spec.description { label.clone().on_hover_text(description); }
                                        if matches!(spec.control, nodecraft_types::ParamControl::Model) {
                                            if nodecraft_nodes::generation::is_generation(&snapshot.descriptor.id) && spec.name == nodecraft_nodes::generation::MODEL {
                                                match shared {
                                                    SharedValue::Mixed => { ui.label("Mixed models"); }
                                                    SharedValue::Uniform(Some(Value::Text(model))) => { ui.label(model); }
                                                    _ => { ui.label("No model selected"); }
                                                }
                                                if ui.button("Change model…").clicked() {
                                                    request=Some(Intent::OpenModel { canvas:snapshot.canvas,exact_ids:snapshot.ids.iter().copied().collect(),revision:snapshot.revision });
                                                }
                                            } else { ui.label("This model parameter has no shared migration picker."); }
                                            return;
                                        }
                                        let mixed = matches!(shared, SharedValue::Mixed) || (matches!(shared, SharedValue::Uniform(None)) && snapshot.defaults_mixed.contains(&spec.name));
                                        let mut intent = None;
                                        if mixed {
                                            ui.label(if snapshot.defaults_mixed.contains(&spec.name) && matches!(shared, SharedValue::Uniform(None)) { "Mixed defaults" } else { "Mixed" });
                                            match &spec.control {
                                                nodecraft_types::ParamControl::Toggle => {
                                                    ui.horizontal(|ui| {
                                                        if ui.button("On for all").clicked() { intent = Some(Some(Value::Bool(true))); }
                                                        if ui.button("Off for all").clicked() { intent = Some(Some(Value::Bool(false))); }
                                                    });
                                                }
                                                nodecraft_types::ParamControl::Choice { .. } | nodecraft_types::ParamControl::Asset { .. } => {
                                                    intent = crate::card::control_with_mode(ui, &spec.control, None, &spec.name, &mut self.editor, label.id, t, true);
                                                }
                                                _ => {
                                                    if !self.editing_mixed.contains(&spec.name) && ui.button("Edit for all…").clicked() {
                                                        self.editing_mixed.insert(spec.name.clone());
                                                        self.editor.begin_empty(&spec.name);
                                                    }
                                                    if self.editing_mixed.contains(&spec.name) {
                                                        intent = crate::card::control_with_mode(ui, &spec.control, None, &spec.name, &mut self.editor, label.id, t, true);
                                                    }
                                                }
                                            }
                                        } else if let SharedValue::Uniform(value) = shared {
                                            if value.is_none() { ui.label(if snapshot.defaults_mixed.contains(&spec.name) { "Mixed defaults" } else { "Default" }); }
                                            intent = crate::card::control_with_mode(ui, &spec.control, value.as_ref().or(spec.default.as_ref()), &spec.name, &mut self.editor, label.id, t, false);
                                        }
                                        if ui.small_button("Reset all to default").clicked() { intent = Some(None); }
                                        if let Some(value) = intent {
                                            request = Some(Intent::Parameters(serde_json::json!({"canvas":snapshot.canvas,"ids":snapshot.ids,"revision":snapshot.revision,"name":spec.name,"value":value})));
                                        }
                                        ui.separator();
                                    }); });
                                }
                            });
                        });
                }
                if let Some(message) = self.error.as_deref().or_else(|| self.editor.error()) { ui.colored_label(t.danger, message); }
            });
        let blocked = shown.is_some_and(|response| ui.input(|i| i.pointer.hover_pos()).is_some_and(|pos| response.response.rect.contains(pos)));
        (blocked, request)
    }
    pub(crate) fn has_uncommitted(&self) -> bool {
        self.editor.has_uncommitted()
    }
    pub(crate) fn targets_dirty(&self, canvas: CanvasId, ids: &[NodeId]) -> bool {
        self.snapshot.as_ref().is_some_and(|snapshot| snapshot.canvas == canvas && ids.iter().any(|id| snapshot.ids.contains(id)))
            && self.editor.has_uncommitted()
    }
    pub fn finish_frame(&mut self, ctx: &egui::Context) {
        if let Some(id) = self.editor.take_focus_request() {
            ctx.memory_mut(|memory| memory.request_focus(id));
            ctx.request_repaint();
        }
    }
    pub fn owns_text_focus(&self, ctx: &egui::Context) -> bool {
        self.editor.owns_text_focus(ctx)
    }
    pub fn owns_popup(&self, ctx: &egui::Context) -> bool {
        self.editor.owns_popup(ctx)
    }
    pub fn failed(&mut self, message: String) {
        self.error = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_types::ProjectId;
    fn session() -> (Session, CanvasId, BTreeSet<NodeId>) {
        let canvas = CanvasId::from_bytes([1; 16]);
        let ids: BTreeSet<_> = [NodeId::from_bytes([2; 16]), NodeId::from_bytes([3; 16])].into_iter().collect();
        let kinds = nodecraft_nodes::Registry::builtins().unwrap();
        let mut document = Project::new(ProjectId::from_bytes([4; 16]), "Public bulk fixture").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        for id in &ids {
            let node = kinds.get("source.text").unwrap().descriptor().instantiate(*id, &Default::default()).unwrap();
            Change::AddNode { canvas, node }.apply(&mut document).unwrap();
        }
        (Session::new(document).unwrap(), canvas, ids)
    }
    #[test]
    fn mixed_is_distinct_from_missing_and_explicit_schema_default() {
        let (mut session, canvas, ids) = session();
        let first = *ids.first().unwrap();
        let args = serde_json::json!({"canvas":canvas,"id":first,"name":"text","value":null});
        session.dispatch("node.param_set", &args).unwrap();
        let snapshot = Snapshot::read(&session, canvas, &ids).unwrap();
        assert_eq!(snapshot.fields.get("text"), Some(&SharedValue::Mixed));
        assert_eq!(snapshot.descriptor.id, "source.text");
        assert!(snapshot.matches(&session, canvas, &ids));
        let args = serde_json::json!({"canvas":canvas,"ids":ids,"revision":session.revision(),"name":"text","value":null});
        session.dispatch("node.params_set_bulk", &args).unwrap();
        assert!(!snapshot.matches(&session, canvas, &ids));
        let snapshot = Snapshot::read(&session, canvas, &ids).unwrap();
        assert_eq!(snapshot.fields.get("text"), Some(&SharedValue::Uniform(None)));
        assert!(!snapshot.matches(&Session::new(session.document().clone()).unwrap(), canvas, &ids));
        assert!(!snapshot.matches(&session, canvas, &BTreeSet::new()));
    }
}
