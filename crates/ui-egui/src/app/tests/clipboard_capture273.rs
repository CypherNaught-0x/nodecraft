//! Opt-in real wgpu captures of the same public scenes as the historical #273 befores.
use super::*;
use std::borrow::Cow;
struct Shape(NodeDescriptor, NodeDescriptor);
impl NodeKind for Shape {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn instance<'a>(&'a self, _: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        Ok(Cow::Borrowed(&self.1))
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
#[test]
#[ignore = "manual public clipboard capture; requires NODECRAFT_273_CAPTURE_DIR"]
fn clipboard273_visual_current() {
    let output = std::path::PathBuf::from(std::env::var("NODECRAFT_273_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let mut measurements = Vec::new();
    for dynamic in [false, true] {
        let short = NodeDescriptor {
            id: "source.text".into(),
            label: "Text".into(),
            category: Category::Source,
            inputs: vec![],
            outputs: vec![],
            params: ParamSchema { params: vec![spec("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, Value::Text(String::new()))] },
            quick_add: None,
        };
        let mut tall = short.clone();
        if dynamic {
            for n in 0..6 {
                tall.params.params.push(spec(&format!("toggle{n}"), ParamControl::Toggle, Value::Bool(false)));
            }
        }
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Clipboard example").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(Shape(short, tall))).unwrap();
        let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let mut h = Harness::builder()
            .with_size(Vec2::new(1000.0, 700.0))
            .with_max_steps(120)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
        h.run();
        let at = if dynamic { Pos2::new(400.0, 300.0) } else { Pos2::new(890.0, 620.0) };
        h.hover_at(at);
        h.event(egui::Event::Paste("A public text example\nReady to edit".into()));
        h.run();
        let app = h.state();
        let scene = app.session.document().canvases().get(&canvas).unwrap();
        assert_eq!(scene.nodes().len(), 1);
        assert_eq!(app.session.revision(), 1);
        assert!(app.session.can_undo());
        let id = *app.selection.nodes().iter().next().unwrap();
        let rect = app.view.rect_to_screen(app.viewport.min, *app.painted.get(&id).unwrap());
        let overlay = crate::navigation::layout(app.viewport, app.navigation.visible, app.theme.tokens()).overlay;
        if dynamic {
            assert!((rect.center() - at).length() < 0.5);
        } else {
            assert!(!overlay.contains(rect.center()));
        }
        let scenario = if dynamic { "dynamic" } else { "minimap" };
        measurements.push(json!({"scenario":scenario,"pointer":[at.x,at.y],"actual_body":[rect.min.x,rect.min.y,rect.max.x,rect.max.y],"center_distance":(rect.center()-at).length(),"nodes":1,"revision":1,"undo":true}));
        h.render().unwrap().save(output.join(format!("{scenario}-after-current.png"))).unwrap();
    }
    std::fs::write(output.join("measurements.json"), serde_json::to_vec_pretty(&measurements).unwrap()).unwrap();
}
