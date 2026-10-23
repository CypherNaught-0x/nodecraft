//! Paired immutable catalog/production-generation definitions (#465): one generation kind per
//! output the catalog's modes make (image; text since #84, 3D since #22, video since #559,
//! trained styles since #167, audio since #672),
//! each looked up by node kind.
use crate::{Session, commands::Resources, run_cmds::RunCatalog};
use nodecraft_catalog::Catalog;
use nodecraft_exec::admission::measure::{add, contract, mul, schema};
use nodecraft_graph::Node;
use nodecraft_nodes::{
    NodeContract, NodeKind,
    generation::{GenerationKind, GenerationMode, GenerationModel, GenerationRole, MODEL, is_generation, kind_for},
};
use nodecraft_types::{Contract, Error, ErrorKind, Modality, ParamOwner, ParamValues};
use std::sync::Arc;
mod wiring;
pub use wiring::GenerationWiring;

const LIMIT: usize = nodecraft_exec::admission::MAX_WITNESS_BYTES;
#[derive(Clone)]
pub struct GenerationCatalog(Arc<Snapshot>);
struct Snapshot {
    catalog: RunCatalog,
    kinds: Vec<GenerationKind>,
}
/// The outputs that get a generation kind, in registration order.
const OUTPUTS: [Modality; 6] = [Modality::Image, Modality::Text, Modality::Mesh, Modality::Video, Modality::Style, Modality::Audio];
#[derive(Clone)]
pub struct GenerationCatalogStamp(RunCatalog);
impl GenerationCatalogStamp {
    /// Bind this runtime snapshot into a consumer preparation token, without exposing a
    /// replacement constructor or minting another identity.
    pub fn bind(&self, hash: &mut blake3::Hasher) {
        hash.update(b"nodecraft.generation-catalog.v1");
        hash.update(&self.0.definition_identity());
    }
}
impl PartialEq for GenerationCatalogStamp {
    fn eq(&self, other: &Self) -> bool {
        self.0.same_snapshot(&other.0)
    }
}
impl Eq for GenerationCatalogStamp {}
impl std::fmt::Debug for GenerationCatalogStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GenerationCatalogStamp(..)")
    }
}
impl GenerationCatalog {
    pub fn builtin() -> Result<Self, Error> {
        Self::new(RunCatalog::new(Catalog::builtin()?)?)
    }
    pub fn new(catalog: RunCatalog) -> Result<Self, Error> {
        let identity = catalog.definition_identity();
        // A model with modes for both outputs is defined, and charged, once per kind.
        let mut charge = 0usize;
        let mut kinds = Vec::new();
        for output in OUTPUTS {
            let models = Self::admit(&catalog, output, &mut charge)?;
            // The image kind is always offered; another kind only once the catalog has a model
            // for it, so the palette never adds a card with nothing to choose (#84).
            match output {
                Modality::Image => kinds.push(GenerationKind::new(models, identity)?),
                _ if models.is_empty() => {}
                Modality::Text => kinds.push(GenerationKind::text(models, identity)?),
                Modality::Video => kinds.push(GenerationKind::video(models, identity)?),
                Modality::Style => kinds.push(GenerationKind::style(models, identity)?),
                Modality::Audio => kinds.push(GenerationKind::audio(models, identity)?),
                _ => kinds.push(GenerationKind::mesh(models, identity)?),
            }
        }
        Ok(Self(Arc::new(Snapshot { catalog, kinds })))
    }
    fn admit(catalog: &RunCatalog, output: Modality, charge: &mut usize) -> Result<Vec<GenerationModel>, Error> {
        // Four simultaneous schema owners (neutral model, instance layout, model contract,
        // validation/constructor copy) plus fixed per-model/mode/role shells. The existing
        // catalog owner is caller-owned; no deep copy precedes this aggregate reservation.
        for m in catalog.catalog().models().filter(|m| m.modes.iter().any(|mode| mode.output == output)) {
            // Admit validation/index scratch before model.validate allocates its indexes.
            *charge = add(*charge, add(64 * 1024, nodecraft_exec::admission::measure::scratch(&m.params)?)?)?;
            if *charge > LIMIT {
                return Err(Error::new(ErrorKind::Budget, "generation definitions exceed the 64 MiB logical metadata limit"));
            }
            m.validate()?;
            *charge = add(*charge, add(m.id.len(), add(m.provider.len(), m.provider_model.len())?)?)?;
            *charge = add(*charge, mul(4, schema(&m.params)?)?)?;
            if let Some(c) = &m.parameter_contract {
                *charge = add(*charge, mul(3, contract(c)?)?)?;
            }
            let mut ports = std::collections::BTreeSet::from(["prompt"]);
            for mode in m.modes.iter().filter(|mode| mode.output == output) {
                for role in &mode.inputs {
                    ports.insert(role.role.as_str());
                    if ports.len() > 256 {
                        return Err(Error::new(ErrorKind::Unsupported, "generation model has too many distinct input roles"));
                    }
                }
            }
            let names = ports.iter().try_fold(0usize, |n, name| add(n, name.len()))?;
            // Each declared mode lists the FULL union of ports, including explicit zeroes
            // for omitted roles. Charge this cross-product before creating ModeContract rows.
            let mode_ports = mul(m.modes.len(), add(mul(ports.len(), 256)?, names)?)?;
            *charge = add(*charge, mul(3, mode_ports)?)?;
            for mode in &m.modes {
                *charge = add(*charge, add(1024, add(mode.id.len(), mul(mode.inputs.len(), 1024)?)?)?)?;
                // Validated above: each listed setting is a distinct model setting.
                let listed = mode.params.iter().flatten().try_fold(0usize, |n, name| add(n, add(name.len(), 64)?))?;
                let audio = mode.audio.as_ref().map_or(0, |audio| audio.role.len().saturating_add(256));
                let audio = audio.saturating_add(mode.edit.as_ref().map_or(0, |edit| edit.role.len().saturating_add(256)));
                *charge = add(*charge, add(listed, audio)?)?;
            }
            if *charge > LIMIT {
                return Err(Error::new(ErrorKind::Budget, "generation definitions exceed the 64 MiB logical metadata limit"));
            }
        }
        let mut models = Vec::new();
        for m in catalog.catalog().models() {
            let modes: Vec<_> = m
                .modes
                .iter()
                .filter(|mode| mode.output == output)
                .map(|mode| {
                    Ok(GenerationMode {
                        id: nodecraft_nodes::ModeId::new(mode.id.clone())?,
                        inputs: mode
                            .inputs
                            .iter()
                            .map(|r| GenerationRole {
                                role: r.role.clone(),
                                modality: r.modality,
                                min: u16::from(r.min),
                                max: u16::from(r.max),
                                inline: r.inline,
                            })
                            .collect(),
                        params: mode.params.clone(),
                        audio: mode.audio.clone(),
                        edit: mode.edit.clone(),
                        document: mode.document.clone(),
                    })
                })
                .collect::<Result<_, Error>>()?;
            if modes.is_empty() {
                continue;
            }
            models.push(GenerationModel {
                id: m.id.clone(),
                provider: m.provider.clone(),
                remote: m.provider_model.clone(),
                parameters: m.parameter_contract.clone().unwrap_or_else(|| Contract::from_schema(m.id.clone(), ParamOwner::Model, m.params.clone())),
                modes,
                style_family: m.style_family.clone(),
            });
        }
        Ok(models)
    }
    /// The generation kind a node of `kind` belongs to.
    fn kind(&self, kind: &str) -> Result<&GenerationKind, Error> {
        self.0.kinds.iter().find(|k| kind_for(k.output()) == Some(kind)).ok_or_else(|| Error::new(ErrorKind::Unsupported, "not a catalog generation kind"))
    }
    /// Current paired resource. Commands retain stamp(), then recheck this pair and every
    /// targeted installed kind before publishing, even when Session revision is unchanged.
    pub fn current(resources: &Resources) -> Result<&Self, Error> {
        let snapshot = resources.require::<Self>()?;
        if !snapshot.matches(resources.require::<RunCatalog>()?) {
            return Err(Error::new(ErrorKind::InvalidInput, "generation catalog snapshot changed; refresh model selection"));
        }
        Ok(snapshot)
    }
    pub fn validate_instance_binding(&self, kind: &dyn NodeKind, node: &Node) -> Result<(), Error> {
        if !is_generation(&node.kind) {
            return Ok(());
        }
        node.validate_payload()?;
        self.validate_definition_binding(kind)
    }
    /// Constant-time runtime binding check; view caches use it without walking node values.
    pub fn validate_definition_binding(&self, kind: &dyn NodeKind) -> Result<(), Error> {
        if kind.definition_snapshot() != Some(self.0.catalog.definition_identity()) {
            return Err(Error::new(ErrorKind::InvalidInput, "generation definitions do not match current catalog snapshot"));
        }
        Ok(())
    }
    pub fn run_catalog(&self) -> RunCatalog {
        self.0.catalog.clone()
    }
    pub fn matches(&self, catalog: &RunCatalog) -> bool {
        self.0.catalog.same_snapshot(catalog)
    }
    pub fn stamp(&self) -> GenerationCatalogStamp {
        GenerationCatalogStamp(self.run_catalog())
    }
    /// Model `id` as a card of `kind` sees it; None when that kind has no such model.
    pub fn model(&self, kind: &str, id: &str) -> Option<&GenerationModel> {
        self.kind(kind).ok()?.model(id)
    }
    /// Every model a card of `kind` can select, by id.
    pub fn models(&self, kind: &str) -> impl Iterator<Item = &GenerationModel> {
        self.kind(kind).ok().into_iter().flat_map(GenerationKind::models)
    }
    pub fn registry(&self) -> Result<nodecraft_nodes::Registry, Error> {
        let mut kinds = nodecraft_nodes::Registry::builtins()?;
        for kind in &self.0.kinds {
            kinds.register_generation(kind.clone())?;
        }
        Ok(kinds)
    }
    pub fn is_model_selector(kind: &str, name: &str) -> bool {
        is_generation(kind) && name == MODEL
    }
    pub fn prepare_instance(&self, existing: &Node, destination: &str, values: &ParamValues) -> Result<Node, Error> {
        self.kind(&existing.kind)?.prepare_instance(existing, destination, values)
    }
    /// Capture only bounded incident port metadata once per target, borrowing the live canvas.
    /// This value is ephemeral; UI controllers retain stamp/IDs instead of this borrow.
    pub fn capture_wiring<'a>(&self, canvas: &'a nodecraft_graph::Canvas, id: nodecraft_types::NodeId) -> Result<GenerationWiring<'a>, Error> {
        GenerationWiring::capture(canvas, id)
    }
    /// Borrow destination metadata for picker filtering; no Node/params/history copies.
    pub fn validate_destination_wiring(&self, kind: &str, wiring: &GenerationWiring<'_>, destination: &str, ready: bool) -> Result<(), Error> {
        let kind = self.kind(kind)?;
        let descriptor = kind.model_descriptor(destination).ok_or_else(|| Error::new(ErrorKind::Unsupported, "destination model is unavailable"))?;
        let contract = kind.model_contract(destination).ok_or_else(|| Error::new(ErrorKind::Unsupported, "destination model contract is unavailable"))?;
        wiring.validate(descriptor, contract, ready)
    }
    pub fn validate_wiring(&self, canvas: &nodecraft_graph::Canvas, candidate: &Node, ready: bool) -> Result<(), Error> {
        let kind = self.kind(&candidate.kind)?;
        kind.validate_instance(candidate, ready)?;
        let contract = kind.instance_contract(candidate)?;
        let descriptor = kind.resolve_instance(candidate)?;
        GenerationWiring::capture(canvas, candidate.id)?.validate(&descriptor, &contract, ready)
    }
    pub fn instance_contract(&self, node: &Node) -> Result<NodeContract, Error> {
        self.kind(&node.kind)?.instance_contract(node)
    }
    pub(crate) fn install(&self, session: &mut Session) {
        session.swap_resource(Some(self.clone()));
        session.swap_resource(Some(self.run_catalog()));
    }
}
/// The one standard registry composition. Explicit custom registries remain caller-owned.
pub fn default_registry() -> Result<(nodecraft_nodes::Registry, GenerationCatalog), Error> {
    let snapshot = GenerationCatalog::builtin()?;
    Ok((snapshot.registry()?, snapshot))
}
/// Check immutable resource pairing and the selected installed definition before quote or
/// launch. Generic resource replacement cannot revive an old UI/migration/run stamp.
pub(crate) fn validate_plan(
    resources: &Resources,
    kinds: &nodecraft_nodes::Registry,
    canvas: &nodecraft_graph::Canvas,
    nodes: impl IntoIterator<Item = nodecraft_types::NodeId>,
) -> Result<(), Error> {
    let mut incoming: Option<Vec<&nodecraft_graph::Edge>> = None;
    for id in nodes {
        let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "planned node is missing"))?;
        if !is_generation(&node.kind) {
            continue;
        }
        let catalog = GenerationCatalog::current(resources)?;
        catalog.validate_instance_binding(kinds.get(&node.kind)?, node)?;
        if incoming.is_none() {
            // One borrowed index for this admission phase, not one full edge scan per node.
            // Reserve before allocating. Counts use <=256 borrowed port-name entries (32 KiB);
            // leave4 MiB of the existing16 MiB scratch ceiling for validation temporaries.
            // The entire index drops before pricing/admission uses its scratch phase.
            let bytes = add(
                std::mem::size_of::<Vec<&nodecraft_graph::Edge>>(),
                add(mul(canvas.edges().len(), std::mem::size_of::<&nodecraft_graph::Edge>())?, 32 * 1024)?,
            )?;
            if bytes > nodecraft_exec::admission::MAX_SCRATCH_BYTES.saturating_sub(4 * 1024 * 1024) {
                return Err(Error::new(ErrorKind::Budget, "generation wire admission exceeds scratch limit"));
            }
            let mut edges = Vec::new();
            edges.try_reserve_exact(canvas.edges().len()).map_err(|_| Error::new(ErrorKind::Budget, "generation wire index allocation refused"))?;
            edges.extend(canvas.edges().values());
            edges.sort_unstable_by_key(|edge| edge.to.node);
            incoming = Some(edges);
        }
        let edges = incoming.as_deref().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "generation wire index is unavailable"))?;
        let start = edges.partition_point(|edge| edge.to.node < id);
        let end = edges.partition_point(|edge| edge.to.node <= id);
        let mut wires = std::collections::BTreeMap::new();
        for edge in edges.get(start..end).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "generation wire range is invalid"))? {
            if !node.inputs.iter().any(|port| port.name == edge.to.port) {
                return Err(Error::new(ErrorKind::InvalidInput, "generation wire reaches an unavailable port"));
            }
            if wires.len() >= 256 && !wires.contains_key(edge.to.port.as_str()) {
                return Err(Error::new(ErrorKind::Budget, "too many generation input ports"));
            }
            let count = wires.entry(edge.to.port.as_str()).or_insert(0usize);
            *count = count.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Budget, "generation wire count overflow"))?;
        }
        // Planning already validates actual endpoint types and the installed kind's hooks.
        // Recheck real wire readiness against the SAME immutable model contract without
        // cloning descriptors/values or substituting a local prompt for a required edge.
        let model = match node.params.get(MODEL) {
            Some(nodecraft_types::Value::Text(id)) => id,
            _ => return Err(Error::new(ErrorKind::InvalidInput, "generation model selection is missing")),
        };
        let contract = catalog.kind(&node.kind)?.model_contract(model).ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                match node.kind.as_str() {
                    nodecraft_nodes::generation::TEXT_KIND => "selected text model is unavailable",
                    nodecraft_nodes::generation::MESH_KIND => "selected 3D model is unavailable",
                    nodecraft_nodes::generation::VIDEO_KIND => "selected video model is unavailable",
                    nodecraft_nodes::generation::STYLE_KIND => "selected style trainer is unavailable",
                    nodecraft_nodes::generation::AUDIO_KIND => "selected audio model is unavailable",
                    _ => "selected image model is unavailable",
                },
            )
        })?;
        contract.resolve_mode(&wires, true)?;
    }
    Ok(())
}
