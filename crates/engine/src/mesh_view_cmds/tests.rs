use super::*;
use crate::Session;
use nodecraft_graph::{Canvas, Project};
use nodecraft_types::ProjectId;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const NODE: NodeId = NodeId::from_bytes([2; 16]);

fn session(kind: &str) -> Session {
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Saved view").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.dispatch("node.create", &json!({"canvas":CANVAS,"id":NODE,"kind":kind,"pos":{"x":0,"y":0}})).unwrap();
    session
}

fn command() -> Value {
    json!({"canvas":CANVAS,"id":NODE,"view":{"yaw":123.456,"pitch":-24.25,"zoom":2.75,"style":"wireframe"}})
}

#[test]
fn mesh928_view_command_is_atomic_undoable_and_serializable() {
    for kind in ["source.mesh", "generation.mesh", "mesh.convert"] {
        let mut session = session(kind);
        let before = session.document().clone();
        session.enabled("mesh.view_set", &command()).unwrap();
        session.dispatch("mesh.view_set", &command()).unwrap();
        let after = session.document().clone();
        let node = after.canvases().get(&CANVAS).unwrap().nodes().get(&NODE).unwrap();
        let state = State::read(&node.params).unwrap();
        assert_eq!((state.yaw, state.pitch, state.zoom, state.style.as_str()), (123.456, -24.25, 2.75, "wireframe"));
        assert_eq!(serde_json::from_slice::<Project>(&serde_json::to_vec(&after).unwrap()).unwrap(), after);
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(session.document(), &before, "all four view settings undo together for {kind}");
        session.dispatch("edit.redo", &json!({})).unwrap();
        assert_eq!(session.document(), &after);
        let revision = session.revision();
        session.dispatch("mesh.view_set", &command()).unwrap();
        assert_eq!(session.revision(), revision, "repeating the saved view is a no-op");
    }
}

#[test]
fn mesh928_bad_view_never_mutates_and_disabled_matches_dispatch() {
    let mut session = session("source.mesh");
    let before = session.document().clone();
    for view in [
        json!({"yaw":-1,"pitch":0,"zoom":1,"style":"shaded"}),
        json!({"yaw":361,"pitch":0,"zoom":1,"style":"shaded"}),
        json!({"yaw":1e300,"pitch":0,"zoom":1,"style":"shaded"}),
        json!({"yaw":0,"pitch":89,"zoom":1,"style":"shaded"}),
        json!({"yaw":0,"pitch":0,"zoom":0,"style":"shaded"}),
        json!({"yaw":0,"pitch":0,"zoom":17,"style":"shaded"}),
        json!({"yaw":0,"pitch":0,"zoom":1,"style":"unknown"}),
        json!({"yaw":0,"pitch":0,"zoom":1,"style":"shaded","extra":true}),
        json!({"yaw":null,"pitch":0,"zoom":1,"style":"shaded"}),
        json!({}),
    ] {
        let params = json!({"canvas":CANVAS,"id":NODE,"view":view});
        assert!(session.enabled("mesh.view_set", &params).is_err());
        assert!(session.dispatch("mesh.view_set", &params).is_err());
        assert_eq!(session.document(), &before);
    }
    let mut session = self::session("source.image");
    let before = session.document().clone();
    assert!(session.dispatch("mesh.view_set", &command()).is_err());
    assert_eq!(session.document(), &before);
}

/// The old descriptor/contract with no presentation controls, independent of the new hash
/// code. Comparing it proves existing paid generation receipts stay fresh on upgrade.
struct Legacy {
    descriptor: nodecraft_nodes::NodeDescriptor,
    contract: nodecraft_nodes::NodeContract,
}
impl nodecraft_nodes::NodeKind for Legacy {
    fn descriptor(&self) -> &nodecraft_nodes::NodeDescriptor {
        &self.descriptor
    }
    fn contract(&self, _: &nodecraft_nodes::NodeDescriptor) -> nodecraft_nodes::NodeContract {
        self.contract.clone()
    }
    fn plan(&self, _: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, Error> {
        Ok(Vec::new())
    }
    fn mode(&self, _: &[nodecraft_types::PortType]) -> Option<nodecraft_nodes::ModeId> {
        None
    }
}

#[test]
fn mesh928_old_paid_hashes_and_submitted_jobs_do_not_change_with_view_settings() {
    use nodecraft_nodes::{InputValue, PlanCtx, mesh::view::NAMES};
    use nodecraft_types::{AssetRef, Blake3, ParamValues, PortType, ProviderTarget, TargetSelection, Value as Param};
    let asset = Param::Media(AssetRef { hash: Blake3::from_bytes([8; 32]), mime: "model/obj".parse().unwrap(), meta: Default::default() });
    for name in ["source.mesh", "mesh.convert", "generation.mesh"] {
        let mut session = session(name);
        let supplied: ParamValues = match name {
            "source.mesh" => [("asset".into(), asset.clone())].into(),
            "generation.mesh" => [("model".into(), Param::Text("tripo.text-to-3d".into())), ("prompt".into(), Param::Text("A blue cube".into()))].into(),
            _ => ParamValues::new(),
        };
        let kind = session.kinds().get(name).unwrap();
        let mut node = kind.instantiate_instance(NODE, Default::default(), &supplied).unwrap();
        let mut descriptor = kind.resolve_instance(&node).unwrap().into_owned();
        let mut contract = kind.instance_contract(&node).unwrap();
        descriptor.params.params.retain(|p| !NAMES.contains(&p.name.as_str()));
        contract.params.schema.params.retain(|p| !NAMES.contains(&p.name.as_str()));
        contract.params.rules.retain(|r| !NAMES.contains(&r.param.as_str()));
        for param in NAMES {
            node.params.remove(param);
        }
        let old_kinds = session.kinds().with_replaced(name, vec![Box::new(Legacy { descriptor, contract })]).unwrap();
        session.dispatch("edit.undo", &json!({})).unwrap();
        session.dispatch("node.add", &json!({"canvas":CANVAS,"node":node})).unwrap();
        let hash = |session: &Session, kinds: &nodecraft_nodes::Registry| {
            nodecraft_exec::staleness::input_hash(session.document().canvases().get(&CANVAS).unwrap(), kinds, NODE).unwrap()
        };
        let legacy = hash(&session, &old_kinds);
        assert_eq!(hash(&session, session.kinds()), legacy, "old {name} recipes remain identical");
        let mut targets = TargetSelection::empty();
        targets.insert(NODE, &ProviderTarget::default_instance("tripo").unwrap()).unwrap();
        let inputs = if name == "mesh.convert" {
            vec![InputValue { port: "model".into(), port_type: PortType { modality: Modality::Mesh, many: false }, value: asset.clone(), origins: vec![] }]
        } else {
            vec![]
        };
        let plan = |session: &Session| {
            let node = session.document().canvases().get(&CANVAS).unwrap().nodes().get(&NODE).unwrap();
            session.kinds().get(name).unwrap().plan(&PlanCtx { node, inputs: &inputs, targets: &targets }).unwrap()
        };
        let jobs = plan(&session);
        session.dispatch("mesh.view_set", &command()).unwrap();
        assert_eq!(hash(&session, session.kinds()), legacy, "view edits never make {name} stale");
        assert_eq!(plan(&session), jobs, "view settings never reach a provider or conversion action");
    }
}
