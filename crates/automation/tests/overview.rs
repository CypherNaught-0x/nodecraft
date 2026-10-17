//! Overview pages through the actual JSON-RPC surface.
use nodecraft_automation::mcp::{McpServer, PAGE_BUDGET_BYTES};
use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Change, Changeset, Project};
use nodecraft_types::{CanvasId, ProjectId};
use serde_json::{Value, json};

fn overview_server(count: u8, title: &str) -> McpServer {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), title).unwrap();
    // Reverse insertion makes the expected id order independent of construction order.
    Changeset::new((1..=count).rev().map(|n| Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([n; 16]), title) }).collect())
        .apply(&mut project)
        .unwrap();
    McpServer::new(Session::new(project).unwrap(), Box::new(|| [9; 16]))
}
fn request(server: &mut McpServer, args: Value) -> (Value, usize) {
    let reply = server.handle(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"canvas_inspect","arguments":args}}).to_string()).unwrap();
    (serde_json::from_str(&reply).unwrap(), reply.len())
}
#[test]
fn escaped_maximum_overview_is_paginated_within_encoded_budget() {
    let mut server = overview_server(64, &"\u{1}".repeat(4096));
    let (reply, wire) = request(&mut server, json!({}));
    let body = &reply["result"]["structuredContent"];
    let structured = serde_json::to_vec(body).unwrap().len();
    eprintln!("escaped64 first-page: structured={structured} wire={wire} canvases={}", body["canvases"].as_array().unwrap().len());
    assert!(structured <= PAGE_BUDGET_BYTES, "overview bypasses encoded page budget: {structured}, wire={wire}");
    assert_eq!(body["project"]["title"], "\u{1}".repeat(4096));
    assert_eq!(body["total"], 64);
    assert_eq!(body["revision"], 0);
    let mut seen = Vec::new();
    let mut offset = 0;
    loop {
        let (reply, wire) = request(&mut server, json!({"offset":offset,"limit":500}));
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let page = &reply["result"]["structuredContent"];
        let structured = serde_json::to_vec(page).unwrap().len();
        eprintln!("escaped64 page offset={offset}: structured={structured} wire={wire} count={}", page["canvases"].as_array().unwrap().len());
        assert!(structured <= PAGE_BUDGET_BYTES);
        // For this fixed small RPC id, JSON-in-text can double each encoded byte,
        // alongside its structured copy. This is measured, not a global RPC wire cap.
        assert!(wire <= 3 * PAGE_BUDGET_BYTES + 256);
        assert_eq!(page["total"], 64);
        assert_eq!(page["revision"], 0);
        let canvases = page["canvases"].as_array().unwrap();
        assert!(!canvases.is_empty());
        seen.extend(canvases.iter().map(|c| c["id"].as_str().unwrap().to_owned()));
        match page["next_offset"].as_u64() {
            Some(next) => {
                assert!(next > offset);
                offset = next;
            }
            None => break,
        }
    }
    let expected: Vec<_> = (1..=64).map(|n| CanvasId::from_bytes([n; 16]).to_string()).collect();
    assert_eq!(seen, expected);
    assert_eq!(server.session().revision(), 0);
}

#[test]
fn overview_validates_paging_and_reports_terminal_empty_and_small_pages() {
    let mut server = overview_server(3, "small");
    let (reply, _) = request(&mut server, json!({}));
    let page = &reply["result"]["structuredContent"];
    assert_eq!(page["project"]["title"], "small");
    assert_eq!(page["canvases"].as_array().unwrap().len(), 3);
    assert_eq!(page["next_offset"], Value::Null);
    let (first, _) = request(&mut server, json!({"limit":2}));
    assert_eq!(first["result"]["structuredContent"]["next_offset"], 2);
    let (last, _) = request(&mut server, json!({"offset":2,"limit":2}));
    assert_eq!(last["result"]["structuredContent"]["canvases"].as_array().unwrap().len(), 1);
    assert_eq!(last["result"]["structuredContent"]["next_offset"], Value::Null);
    for offset in [3, 99, usize::MAX] {
        let (reply, _) = request(&mut server, json!({"offset":offset}));
        let page = &reply["result"]["structuredContent"];
        assert_eq!(page["canvases"], json!([]));
        assert_eq!(page["next_offset"], Value::Null);
        assert_eq!(page["total"], 3);
    }
    for args in [
        json!({"offset":-1}),
        json!({"offset":1.5}),
        json!({"offset":"0"}),
        json!({"offset":null}),
        serde_json::from_str(r#"{"offset":18446744073709551616}"#).unwrap(),
        json!({"limit":0}),
        json!({"limit":501}),
        json!({"limit":-1}),
        json!({"limit":"2"}),
        json!({"limit":1.5}),
        json!({"limit":null}),
        json!({"limit":u64::MAX}),
        json!({"node":CanvasId::from_bytes([1;16])}),
        json!({"edge_offset":0}),
    ] {
        let (reply, _) = request(&mut server, args.clone());
        assert_eq!(reply["result"]["isError"], true, "{args}: {reply}");
    }
    let mut empty = overview_server(0, "empty");
    let (reply, _) = request(&mut empty, json!({}));
    assert_eq!(reply["result"]["structuredContent"]["canvases"], json!([]));
    assert_eq!(reply["result"]["structuredContent"]["total"], 0);
    assert_eq!(reply["result"]["structuredContent"]["next_offset"], Value::Null);
}
