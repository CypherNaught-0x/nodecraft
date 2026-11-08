//! AWS's published SigV4 test suite, as kept in `awslabs/aws-c-auth`
//! (`tests/aws-signing-test-suite/v4` at bdb7cef99d8bd1ad3c189b1400140a7787e47105, Apache-2.0;
//! see ATTRIBUTION.md). The cases use AWS's public example credentials (`AKIDEXAMPLE`), never
//! real ones. Left out: the path-normalising cases (`get-slash-normalized` and the relative-path
//! ones), since the signer doesn't normalise; the unnormalised `//` case is in.

use super::*;
use serde_json::Value;

struct Case {
    name: &'static str,
    context: &'static str,
    request: &'static str,
    canonical: &'static str,
    string_to_sign: &'static str,
    signature: &'static str,
}

macro_rules! case {
    ($name:literal) => {
        Case {
            name: $name,
            context: include_str!(concat!("../fixtures/sigv4/", $name, "/context.json")),
            request: include_str!(concat!("../fixtures/sigv4/", $name, "/request.txt")),
            canonical: include_str!(concat!("../fixtures/sigv4/", $name, "/header-canonical-request.txt")),
            string_to_sign: include_str!(concat!("../fixtures/sigv4/", $name, "/header-string-to-sign.txt")),
            signature: include_str!(concat!("../fixtures/sigv4/", $name, "/header-signature.txt")),
        }
    };
}

fn cases() -> Vec<Case> {
    vec![
        case!("get-header-value-trim"),
        case!("get-slash-unnormalized"),
        case!("get-space-normalized"),
        case!("get-space-unnormalized"),
        case!("get-unreserved"),
        case!("get-utf8"),
        case!("get-vanilla"),
        case!("get-vanilla-empty-query-key"),
        case!("get-vanilla-query"),
        case!("get-vanilla-query-order-key-case"),
        case!("get-vanilla-query-unreserved"),
        case!("get-vanilla-utf8-query"),
        case!("get-vanilla-with-session-token"),
        case!("post-header-key-case"),
        case!("post-header-key-sort"),
        case!("post-header-value-case"),
        case!("post-sts-header-after"),
        case!("post-sts-header-before"),
        case!("post-vanilla"),
        case!("post-vanilla-query"),
        case!("post-x-www-form-urlencoded"),
        case!("post-x-www-form-urlencoded-parameters"),
    ]
}

/// The canonical request, string to sign and signature for one case, signed as the suite says.
fn sign_case(case: &Case) -> (String, String, String) {
    let context: Value = serde_json::from_str(case.context).unwrap();
    let text = |path: &str| context.pointer(path).and_then(Value::as_str).unwrap().to_owned();
    let flag = |key: &str| context.get(key).and_then(Value::as_bool).unwrap_or(false);
    let (head, body) = case.request.split_once("\n\n").unwrap_or((case.request, ""));
    let mut lines = head.lines();
    let line = lines.next().unwrap();
    let (method, target) = line.split_once(' ').unwrap();
    let target = target.strip_suffix(" HTTP/1.1").unwrap();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers: Vec<(String, String)> = lines.map(|l| l.split_once(':').unwrap()).map(|(n, v)| (n.to_owned(), v.to_owned())).collect();
    let now = crate::aws::parse_expiry(&text("/timestamp")).unwrap();
    let (amz, date) = amz_date(now);
    headers.push(("X-Amz-Date".into(), amz.clone()));
    if let Some(token) = context.pointer("/credentials/token").and_then(Value::as_str)
        && !flag("omit_session_token")
    {
        headers.push(("X-Amz-Security-Token".into(), token.to_owned()));
    }
    let payload_hash = sha256_hex(body.as_bytes());
    if flag("sign_body") {
        headers.push(("X-Amz-Content-Sha256".into(), payload_hash.clone()));
    }
    let (canonical, _) = Canonical { method, path, query, headers: &headers, payload_hash: &payload_hash }.request();
    let scope = scope(&date, &text("/region"), &text("/service"));
    let to_sign = string_to_sign(&amz, &scope, &canonical);
    let signature = signature(&text("/credentials/secret_access_key"), &date, &text("/region"), &text("/service"), &to_sign).unwrap();
    (canonical, to_sign, signature)
}

#[test]
fn passes_the_aws_sigv4_test_suite() {
    let cases = cases();
    assert_eq!(cases.len(), 22);
    for case in &cases {
        let (canonical, to_sign, signature) = sign_case(case);
        assert_eq!(canonical, case.canonical.trim_end_matches('\n'), "{}: canonical request", case.name);
        assert_eq!(to_sign, case.string_to_sign.trim_end_matches('\n'), "{}: string to sign", case.name);
        assert_eq!(signature, case.signature.trim(), "{}: signature", case.name);
    }
}

#[test]
fn the_canonical_uri_encodes_a_sent_path_again() {
    // A Bedrock model id's `:` is sent as `%3A`; for every service but S3 AWS signs the sent
    // path encoded once more.
    assert_eq!(
        canonical_uri("/model/us.anthropic.claude-haiku-4-5-20251001-v1%3A0/converse"),
        "/model/us.anthropic.claude-haiku-4-5-20251001-v1%253A0/converse"
    );
    // An ARN's `:` and `/` are both inside one sent segment.
    assert_eq!(
        canonical_uri("/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123456789012%3Ainference-profile%2Fus.anthropic.claude-haiku-4-5-20251001-v1%3A0/converse"),
        "/model/arn%253Aaws%253Abedrock%253Aus-east-1%253A123456789012%253Ainference-profile%252Fus.anthropic.claude-haiku-4-5-20251001-v1%253A0/converse"
    );
    assert_eq!(canonical_uri(""), "/");
    assert_eq!(canonical_uri("/"), "/");
    assert_eq!(uri_encode(b"a b+c/~", false), "a%20b%2Bc%2F~");
    assert_eq!(uri_encode("ሴ".as_bytes(), true), "%E1%88%B4");
}

#[test]
fn query_names_and_values_are_decoded_then_encoded_and_sorted() {
    assert_eq!(canonical_query("b=2&a=%7e1&a=0&c"), "a=0&a=~1&b=2&c=");
    // A stray `%` is a literal percent sign.
    assert_eq!(canonical_query("x=100%&y=%zz"), "x=100%25&y=%25zz");
    assert_eq!(canonical_query(""), "");
}

#[test]
fn dates_come_from_unix_seconds() {
    assert_eq!(amz_date(0), ("19700101T000000Z".into(), "19700101".into()));
    assert_eq!(amz_date(1_440_938_160), ("20150830T123600Z".into(), "20150830".into()));
    // A leap day, and the end of a century that isn't a leap year's.
    assert_eq!(amz_date(951_782_400).1, "20000229");
    let feb_end = u64::try_from(days_from_civil(2100, 3, 1) - 1).unwrap() * 86_400;
    assert_eq!(amz_date(feb_end).1, "21000228");
    for days in [0_i64, 59, 365, 10_957, 16_677, 47_481, 2_932_896] {
        let (y, m, d) = civil_from_days(days);
        assert_eq!(days_from_civil(y, m, d), days);
    }
    // No overflow at the clock's end.
    let _ = amz_date(u64::MAX);
}
