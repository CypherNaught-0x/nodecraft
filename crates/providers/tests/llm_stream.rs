//! Streamed chat through the real reqwest transport against a local server speaking each dialect.

#![cfg(all(feature = "http", feature = "llm", not(target_arch = "wasm32")))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use nodecraft_providers::ReqwestHttp;
use nodecraft_providers::llm::{ApiKey, ChatRequest, Endpoint, LlmClient, Message, StopReason};

/// Serves one response, writing the body in small pieces with flushes in between.
fn serve(status: &str, content_type: &str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let status = status.to_owned();
    let content_type = content_type.to_owned();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else { return };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut req = vec![0; len];
        let _ = reader.read_exact(&mut req);
        let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n");
        for piece in body.as_bytes().chunks(7) {
            let _ = stream.write_all(piece);
            let _ = stream.flush();
        }
    });
    url
}

fn req(model: &str) -> ChatRequest {
    ChatRequest { model: model.into(), system: None, messages: vec![Message::user("hi")], max_tokens: 50, temperature: None }
}

#[tokio::test(flavor = "current_thread")]
async fn openai_compatible_stream() {
    let base = serve(
        "200 OK",
        "text/event-stream",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo!\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    let client = LlmClient::new(Endpoint::openai_compatible("vllm", &format!("{base}/v1"), None), Arc::new(ReqwestHttp::new().unwrap()));
    let mut pieces = Vec::new();
    let reply = client.chat_stream(&req("m"), &mut |t| pieces.push(t.to_owned())).await.unwrap();
    assert_eq!(reply.text, "Hello!");
    assert_eq!(pieces.concat(), "Hello!");
    assert_eq!(reply.stop, StopReason::EndTurn);
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_stream() {
    let base = serve(
        "200 OK",
        "text/event-stream",
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Bonjour\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let mut endpoint = Endpoint::anthropic(ApiKey::new("test"));
    endpoint.base_url = format!("{base}/v1");
    let client = LlmClient::new(endpoint, Arc::new(ReqwestHttp::new().unwrap()));
    let reply = client.chat_stream(&req("claude-opus-5-5"), &mut |_| {}).await.unwrap();
    assert_eq!((reply.text.as_str(), reply.input_tokens, reply.output_tokens), ("Bonjour", Some(5), Some(3)));
}

#[tokio::test(flavor = "current_thread")]
async fn a_stream_cut_short_is_an_error_and_error_statuses_keep_their_body() {
    let base = serve("200 OK", "text/event-stream", "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n");
    let client = LlmClient::new(Endpoint::openai_compatible("x", &format!("{base}/v1"), None), Arc::new(ReqwestHttp::new().unwrap()));
    let e = client.chat_stream(&req("m"), &mut |_| {}).await.unwrap_err();
    assert!(e.is_transient(), "{e}");

    let base = serve("429 Too Many Requests", "application/json", "{\"error\":{\"message\":\"quota\",\"code\":\"insufficient_quota\"}}");
    let client = LlmClient::new(Endpoint::openai_compatible("x", &format!("{base}/v1"), None), Arc::new(ReqwestHttp::new().unwrap()));
    let e = client.chat_stream(&req("m"), &mut |_| {}).await.unwrap_err();
    assert_eq!(e.kind, nodecraft_providers::ErrorKind::Budget);
}
