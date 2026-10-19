//! Source-aware model choice over the actual JSON-RPC/MCP command surface.
use nodecraft_automation::mcp::{AGENT_COMMANDS, McpServer};
use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, NodeId, ParamValues, ProjectId, Value};
use serde_json::json;
const C: CanvasId = CanvasId::from_bytes([157; 16]);
const A: NodeId = NodeId::from_bytes([1; 16]);
const B: NodeId = NodeId::from_bytes([2; 16]);
const WRONG: NodeId = NodeId::from_bytes([3; 16]);
const SOURCE: &str = "openai-images.sunburst";
const DESTINATION: &str = "openai-images.flare";
fn server() -> McpServer {
    let mut project = Project::new(ProjectId::from_bytes([157; 16]), "MCP model choice").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let seed = Session::new(project.clone()).unwrap();
    let values: ParamValues = [("model".into(), Value::Text(SOURCE.into())), ("prompt".into(), Value::Text("Public-safe prompt".into()))].into();
    for id in [A, B] {
        let node = seed.kinds().get("generation.image").unwrap().instantiate_instance(id, Default::default(), &values).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    let node = seed.kinds().get("source.text").unwrap().instantiate_instance(WRONG, Default::default(), &Default::default()).unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    McpServer::new(Session::new(project).unwrap(), Box::new(|| [7; 16]))
}
fn tool(s: &mut McpServer, name: &str, args: serde_json::Value) -> serde_json::Value {
    let reply = s.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}).to_string()).unwrap();
    serde_json::from_str::<serde_json::Value>(&reply).unwrap()["result"].clone()
}
fn command(s: &mut McpServer, id: &str, params: serde_json::Value) -> serde_json::Value {
    tool(s, "command_run", json!({"id":id,"params":params}))
}

#[test]
fn listed_model_commands_preview_then_apply_all_targets_as_one_undo() {
    let mut s = server();
    let list = tool(&mut s, "command_list", json!({"query":"model"}));
    for id in ["node.model_preview", "node.set_model"] {
        assert!(AGENT_COMMANDS.contains(&id));
        assert!(list["structuredContent"]["commands"].as_array().unwrap().iter().any(|entry| entry["id"] == id && entry["params_schema"].is_object()));
    }
    let before = s.session().unwrap().document().clone();
    let p = command(&mut s, "node.model_preview", json!({"canvas":C,"nodes":[A,B],"model":DESTINATION}));
    assert_eq!(p["isError"], false, "{p}");
    let p = &p["structuredContent"]["result"];
    assert_eq!(s.session().unwrap().document(), &before);
    let changed = command(
        &mut s,
        "node.set_model",
        json!({"canvas":C,"nodes":[A,B],"model":DESTINATION,"expected_revision":p["revision"],"preview_binding":p["binding"]}),
    );
    assert_eq!(changed["isError"], false, "{changed}");
    assert_eq!(s.session().unwrap().revision(), 1);
    for id in [A, B] {
        assert_eq!(
            s.session().unwrap().document().canvases().get(&C).unwrap().nodes().get(&id).unwrap().params.get("model"),
            Some(&Value::Text(DESTINATION.into()))
        );
    }
    assert_eq!(command(&mut s, "edit.undo", json!({}))["isError"], false);
    assert_eq!(s.session().unwrap().document(), &before);
}
#[test]
fn mcp_model_refusals_keep_document_revision_history_and_events_atomic() {
    let mut s = server();
    let before = s.session().unwrap().document().clone();
    for (id, params) in [
        ("node.model_preview", json!({"canvas":C,"nodes":[A,WRONG],"model":DESTINATION})),
        ("node.model_preview", json!({"canvas":C,"nodes":[A,A],"model":DESTINATION})),
        ("node.param_set", json!({"canvas":C,"id":A,"name":"model","value":Value::Text(DESTINATION.into())})),
        ("node.set_model", json!({"canvas":C,"nodes":[A,B],"model":DESTINATION,"expected_revision":0,"preview_binding":"0".repeat(64)})),
    ] {
        assert_eq!(command(&mut s, id, params)["isError"], true);
        let session = s.session().unwrap();
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
        assert!(session.events_since(0).unwrap().is_empty());
    }
}
#[test]
fn mcp_old_preview_cannot_be_replayed_after_an_intervening_edit() {
    let mut s = server();
    let p = command(&mut s, "node.model_preview", json!({"canvas":C,"nodes":[A],"model":DESTINATION}));
    assert_eq!(command(&mut s, "node.title", json!({"canvas":C,"id":B,"title":"New revision"}))["isError"], false);
    let before = s.session().unwrap().document().clone();
    let p = &p["structuredContent"]["result"];
    let result =
        command(&mut s, "node.set_model", json!({"canvas":C,"nodes":[A],"model":DESTINATION,"expected_revision":p["revision"],"preview_binding":p["binding"]}));
    assert_eq!(result["isError"], true);
    assert_eq!(s.session().unwrap().document(), &before);
}
