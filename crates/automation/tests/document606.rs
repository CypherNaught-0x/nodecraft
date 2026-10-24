//! Real public PDF content reaches the same edit boundary over both headless transports.
use nodecraft_automation::{blank_session, mcp::McpServer};
use nodecraft_engine::{
    Session,
    import::{Incoming, import_files},
};
use nodecraft_graph::Position;
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::{Value, json};

const C: CanvasId = CanvasId::from_bytes([61; 16]);
const PDF: NodeId = NodeId::from_bytes([62; 16]);
const NEW: NodeId = NodeId::from_bytes([63; 16]);

fn session() -> Session {
    let mut s = blank_session(ProjectId::from_bytes([60; 16]), C).unwrap();
    let bytes = nodecraft_media::pdf_fixture::pdf(&["BT /F1 12 Tf 20 150 Td (Public transport extraction) Tj ET"]);
    import_files(&mut s, C, Position::default(), 0.0, &[Incoming { name: "public.pdf", bytes: &bytes }], &[PDF]).unwrap();
    s
}

fn call(server: &mut McpServer, tool: &str, args: Value) -> Value {
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    let reply: Value = serde_json::from_str(&server.handle(&request.to_string()).unwrap()).unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"].clone()
}

#[test]
fn mcp_lists_and_executes_both_extractions_without_a_dialog_or_file_capability() {
    for command in ["document.extract_page", "document.extract_text"] {
        let mut server = McpServer::new(session(), Box::new(|| [64; 16]));
        let listed = call(&mut server, "command_list", json!({"query":"document.extract"}));
        let commands = listed["structuredContent"]["commands"].as_array().unwrap();
        assert!(commands.iter().any(|c| c["id"] == command), "{listed}");
        let before = server.session().unwrap().document().clone();
        let revision = server.session().unwrap().revision();
        let result = call(&mut server, "command_run", json!({"id":command,"params":{"canvas":C,"node":PDF,"page":1,"id":NEW}}));
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["structuredContent"]["result"]["id"], json!(NEW));
        let inspect = call(&mut server, "canvas_inspect", json!({"canvas":C,"node":NEW}));
        let made = &inspect["structuredContent"]["node"];
        if command.ends_with("text") {
            assert_eq!(made["kind"], "source.text");
            assert_eq!(made["params"]["text"]["value"], "Public transport extraction");
        } else {
            assert_eq!(made["kind"], "source.document");
            assert_eq!(made["params"]["asset"]["value"]["meta"]["page_count"], 1);
        }
        assert_eq!(server.session().unwrap().events_since(revision).unwrap().len(), 1);
        assert_eq!(call(&mut server, "command_run", json!({"id":"edit.undo"}))["isError"], false);
        assert_eq!(server.session().unwrap().document(), &before);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn cli_control_protocol_extracts_and_undoes_in_the_seeded_headless_session() {
    for command in ["document.extract_page", "document.extract_text"] {
        let mut s = session();
        let before = s.document().clone();
        let requests = [
            json!({"id":1,"method":"command.run","params":{"id":command,"params":{"canvas":C,"node":PDF,"page":1,"id":NEW}}}),
            json!({"id":2,"method":"canvas.inspect","params":{"canvas":C,"node":NEW}}),
            json!({"id":3,"method":"command.run","params":{"id":"edit.undo","params":{}}}),
        ];
        let input = requests.iter().map(|v| format!("{v}\n")).collect::<String>();
        let mut output = Vec::new();
        nodecraft_automation::control::stdio::serve(&mut s, &mut std::io::Cursor::new(input), &mut output).unwrap();
        let replies = String::from_utf8(output).unwrap().lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();
        assert_eq!(replies.len(), 3);
        assert!(replies.iter().all(|r| r.get("error").is_none()), "{replies:?}");
        assert_eq!(replies[0]["result"]["result"]["id"], json!(NEW));
        assert_eq!(replies[1]["result"]["node"]["id"], json!(NEW));
        assert_eq!(s.document(), &before);
    }
}
