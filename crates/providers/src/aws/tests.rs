//! AWS credentials and the signing transport, against public example credentials only.

use super::*;
use crate::http::{Http, HttpRequest, HttpResponse};
use crate::provider::BoxFuture;
use futures::executor::block_on;
use std::sync::{Arc, Mutex};

/// AWS's published example credentials (the SigV4 test suite's), never real ones.
pub(crate) const ACCESS: &str = "AKIDEXAMPLE";
pub(crate) const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
/// The test suite's example session token.
pub(crate) const TOKEN: &str = "AQoDYXdzEPT//////////wEXAMPLEtc764bNrC9SAPBSM22wDOk4x4HIZ8j4FZTwdQWLWsKWHGBuFqwAeMicRXmxfpSPfIeoIYRqTflfKD8YUuwthAx7mSEI/qkPpKPi/kMcGdQrmGdeehM4IC1NtBmUpp2wUE8phUZampKsburEDy0KPkyQDYwT7WZ0wq5VSXDvp75YU9HFvlRd8Tx6q6fE8YQcHNVXAkiY9q6d+xo0rKwT38xVqr7ZD0u0iPPkUL64lIZbqBAz+scqKmlzm8FDrypNC9Yjc8fPOLn9FX9KSYvKTr4rvx3iSIlTJabIQwj2ICCR/oLxBA==";
/// 2015-08-30T12:36:00Z, the suite's time.
pub(crate) fn suite_time() -> Option<u64> {
    Some(1_440_938_160)
}
const HOST: &str = "bedrock-runtime.us-east-1.amazonaws.com";
const BODY: &[u8] = br#"{"messages":[{"role":"user","content":[{"text":"Hello"}]}]}"#;

/// Answers every request with one reply, recording what it was sent.
pub(crate) struct Recorder {
    reply: HttpResponse,
    pub(crate) seen: Mutex<Vec<HttpRequest>>,
}

impl Recorder {
    pub(crate) fn new(status: u16, body: &str) -> Arc<Self> {
        Arc::new(Self { reply: HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() }, seen: Mutex::new(vec![]) })
    }
}

impl Http for Recorder {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, crate::ProviderError>> {
        self.seen.lock().unwrap().push(request);
        let reply = self.reply.clone();
        Box::pin(async move { Ok(reply) })
    }
}

fn signer(http: Arc<dyn Http>, token: Option<&str>) -> SigV4Http {
    SigV4Http::new(http, AwsCredentials::new(ACCESS, SECRET, token).unwrap(), "us-east-1", "bedrock", HOST).with_clock(suite_time)
}

fn converse(path: &str) -> HttpRequest {
    let mut request = HttpRequest::get(format!("https://{HOST}{path}")).header("Content-Type", "application/json");
    request.method = crate::http::HttpMethod::Post;
    request.body = BODY.to_vec();
    request
}

fn header<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Cross-checked against botocore 1.35.99's `SigV4Auth` (service `bedrock`, us-east-1, the suite's
/// example credentials and time, a `Content-Type: application/json` header and
/// `X-Amz-Content-SHA256` set to the body's hash), run 2026-11-08: its canonical request had the
/// same doubly encoded path and signed headers, and these signatures.
#[test]
fn bedrock_requests_sign_as_botocore_does() {
    let http = Recorder::new(200, "{}");
    let signed = signer(http.clone(), None).sign(converse("/model/us.anthropic.claude-haiku-4-5-20251001-v1%3A0/converse")).unwrap();
    assert_eq!(
        header(&signed, "authorization"),
        Some(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, Signature=2d9aa1d97f96a71923a3ce0ab3c5b060785f6df55d41f0720c17cb901d4077a3"
        )
    );
    assert_eq!(header(&signed, "x-amz-date"), Some("20150830T123600Z"));
    assert_eq!(header(&signed, "x-amz-content-sha256"), Some("7421da8d1a0949a481724fee62d5886aacde03836bf58248adb73e8b61ac0d54"));
    assert!(header(&signed, "x-amz-security-token").is_none());

    let arn = "/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123456789012%3Ainference-profile%2Fus.anthropic.claude-haiku-4-5-20251001-v1%3A0/converse-stream";
    let signed = signer(http, Some(TOKEN)).sign(converse(arn).header("Accept", "application/vnd.amazon.eventstream")).unwrap();
    assert_eq!(
        header(&signed, "authorization"),
        Some(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token, Signature=46c8c3708f5a2ceffdcc197b9a661502b959a0648a727d142d4900d76059a1e5"
        ),
        "Accept is sent but not signed"
    );
    assert_eq!(header(&signed, "x-amz-security-token"), Some(TOKEN));
    for secret in ["authorization", "x-amz-security-token"] {
        assert!(signed.secret_headers.iter().any(|n| n == secret), "{secret} never follows a redirect off the host");
    }
}

#[test]
fn headers_the_caller_set_are_replaced_not_doubled() {
    let request = converse("/model/m/converse").header("Authorization", "Bearer stale").header("X-Amz-Date", "19990101T000000Z");
    let signed = signer(Recorder::new(200, "{}"), None).sign(request).unwrap();
    let count = |name: &str| signed.headers.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).count();
    assert_eq!((count("authorization"), count("x-amz-date")), (1, 1));
    assert!(header(&signed, "authorization").unwrap().starts_with("AWS4-HMAC-SHA256 "));
}

#[test]
fn only_the_one_https_host_is_signed() {
    let http = Recorder::new(200, "{}");
    let signer = signer(http.clone(), Some(TOKEN));
    for url in [
        "http://bedrock-runtime.us-east-1.amazonaws.com/model/m/converse",
        "https://bedrock-runtime.us-west-2.amazonaws.com/model/m/converse",
        "https://evil.example/model/m/converse",
        "https://bedrock-runtime.us-east-1.amazonaws.com.evil.example/x",
        "https://bedrock-runtime.us-east-1.amazonaws.com:8443/x",
        "https://user:pass@bedrock-runtime.us-east-1.amazonaws.com/x",
        "not a url",
    ] {
        let error = block_on(signer.send(HttpRequest::get(url))).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::UnsupportedInput, "{url}");
        assert!(!error.message.contains(TOKEN) && !error.message.contains(SECRET));
    }
    assert!(http.seen.lock().unwrap().is_empty(), "nothing was sent");
}

#[test]
fn expired_or_unclocked_credentials_are_refused_before_sending() {
    let http = Recorder::new(200, "{}");
    let now = suite_time().unwrap();
    let credentials = AwsCredentials::new(ACCESS, SECRET, Some(TOKEN)).unwrap();
    for (expiry, refused) in [(now, true), (now + EXPIRY_MARGIN_S, true), (now + EXPIRY_MARGIN_S + 1, false), (now + 3_600, false)] {
        let signer = SigV4Http::new(http.clone(), credentials.clone().with_expiry(expiry), "us-east-1", "bedrock", HOST).with_clock(suite_time);
        let result = block_on(signer.send(converse("/model/m/converse")));
        assert_eq!(result.is_err(), refused, "{expiry}");
        if let Err(error) = result {
            assert_eq!(error.kind, crate::ErrorKind::Auth);
            assert!(error.message.contains("expired"), "{}", error.message);
        }
    }
    assert_eq!(http.seen.lock().unwrap().len(), 2, "only the fresh ones were sent");
    let unclocked = SigV4Http::new(http.clone(), credentials, "us-east-1", "bedrock", HOST).with_clock(|| None);
    assert_eq!(block_on(unclocked.send(converse("/model/m/converse"))).unwrap_err().kind, crate::ErrorKind::Auth);
}

#[test]
fn an_error_reply_quoting_the_session_token_has_it_removed() {
    let quoted = format!(r#"{{"message":"The request signature we calculated does not match. Canonical String: x-amz-security-token:{TOKEN} and {TOKEN}"}}"#);
    let http = Recorder::new(403, &quoted);
    let reply = block_on(signer(http, Some(TOKEN)).send(converse("/model/m/converse"))).unwrap();
    let body = String::from_utf8(reply.body).unwrap();
    assert!(!body.contains(TOKEN), "{body}");
    assert_eq!(body.matches("[redacted]").count(), 2);
    // A successful reply is passed on as it came.
    let http = Recorder::new(200, TOKEN);
    assert_eq!(block_on(signer(http, Some(TOKEN)).send(converse("/model/m/converse"))).unwrap().body, TOKEN.as_bytes());
}

#[test]
fn credentials_are_checked_and_never_printed() {
    let credentials = AwsCredentials::new(&format!(" {ACCESS}AAAAA "), SECRET, Some(TOKEN)).unwrap().with_expiry(9);
    let printed = format!("{credentials:?} {:?}", signer(Recorder::new(200, "{}"), Some(TOKEN)));
    for secret in [ACCESS, SECRET, TOKEN] {
        assert!(!printed.contains(secret), "{printed}");
    }
    assert!(credentials.has_session_token());
    assert_eq!(credentials.expires_at(), Some(9));
    let long = "A".repeat(129);
    for (access, secret, token) in [
        ("", SECRET, None),
        ("AKID-EXAMPLE", SECRET, None),
        (long.as_str(), SECRET, None),
        (ACCESS, "", None),
        (ACCESS, "has space", None),
        (ACCESS, SECRET, Some("")),
        (ACCESS, SECRET, Some("bad\ntoken")),
    ] {
        let error = AwsCredentials::new(access, secret, token).unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Auth);
        assert!(!error.message.contains(SECRET) && !error.message.contains("has space"), "{}", error.message);
    }
}

#[test]
fn expiries_are_rfc3339_times() {
    assert_eq!(parse_expiry("2015-08-30T12:36:00Z").unwrap(), 1_440_938_160);
    assert_eq!(parse_expiry(" 2015-08-30T14:36:00.123+02:00 ").unwrap(), 1_440_938_160);
    assert_eq!(parse_expiry("2015-08-30t08:06:00-04:30").unwrap(), 1_440_938_160);
    assert_eq!(parse_expiry("2015-08-30 12:36:00z").unwrap(), 1_440_938_160);
    assert_eq!(parse_expiry("1970-01-01T00:00:00Z").unwrap(), 0);
    assert_eq!(parse_expiry("2024-02-29T00:00:00Z").unwrap(), 1_709_164_800);
    for bad in [
        "",
        "2015-08-30",
        "2015-08-30T12:36:00",
        "2015-08-30T12:36Z",
        "2015-02-29T00:00:00Z",
        "2015-13-01T00:00:00Z",
        "2015-08-30T24:00:00Z",
        "2015-08-30T12:36:00+2:00",
        "2015-08-30T12:36:00.Z",
        "1969-12-31T23:59:59Z",
        "1970-01-01T00:00:00+00:01",
        "+015-08-30T12:36:00Z",
        "2015-08-30T12:36:00Zjunk",
        "２０15-08-30T12:36:00Z",
        "1440938160",
    ] {
        assert!(parse_expiry(bad).is_err(), "{bad:?}");
    }
}
