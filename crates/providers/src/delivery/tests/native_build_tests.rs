use super::*;
use crate::ReqwestHttp;
use std::net::TcpListener;

async fn classification(streaming: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let request = HttpRequest::get(format!("http://{}/private-url-marker?private-query-marker", listener.local_addr().unwrap()))
        .secret_header("authorization", "public-header\nprivate-key-marker");
    let http = ReqwestHttp::new().unwrap();
    let context = SubmitContext::new();
    context.arm();
    let child = Child::new(Arc::clone(&context.state)).unwrap();
    let failure = if streaming {
        let mut sink = |_: &[u8]| Ok(());
        http.send_streaming_with_delivery(request, &mut sink, &child.context).await.unwrap_err()
    } else {
        http.send_with_delivery(request, &child.context).await.unwrap_err()
    };
    let error = child.finish(Err(failure)).unwrap_err();
    assert!(context.finish_error().is_some(), "native build rejection preserves existing positive evidence");
    assert!(matches!(listener.accept(),Err(e) if e.kind()==std::io::ErrorKind::WouldBlock), "no loopback connection reached the listener");
    assert_eq!(error.kind, ErrorKind::UnsupportedInput);
    assert!(!error.is_transient());
    let text = error.to_string();
    assert!(text.len() < 160);
    for marker in ["private-url-marker", "private-query-marker", "private-key-marker", "public-header"] {
        assert!(!text.contains(marker), "{text}");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn native_request_build_ordinary_is_terminal_and_safe() {
    classification(false).await;
}
#[tokio::test(flavor = "current_thread")]
async fn native_request_build_streaming_is_terminal_and_safe() {
    classification(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_execute_invocation_connect_and_total_timeout_stay_transient_unknown() {
    let http = ReqwestHttp::new().unwrap();
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    let context = SubmitContext::new();
    context.arm();
    let error = context.send(&http, HttpRequest::get(format!("http://{address}")).timeout_ms(500)).await.unwrap_err();
    // The bounded closed-port attempt can hit its deadline before the platform reports
    // connection refusal. Both outcomes follow execute and remain transient/unknown.
    assert!(matches!(error.kind, ErrorKind::Network | ErrorKind::Timeout));
    assert!(error.is_transient());
    assert!(context.finish_error().is_none());
    // The total request deadline can expire before the transport is first polled.
    // This checks timeout classification and conservative evidence, not TCP acceptance.
    // Actual HTTP execution is independently covered by the engine POST controls.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let context = SubmitContext::new();
    context.arm();
    let error = context.send(&http, HttpRequest::get(format!("http://{}", listener.local_addr().unwrap())).timeout_ms(50)).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(error.is_transient());
    assert!(context.finish_error().is_none());
}
