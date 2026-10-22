//! Fixtures shared by the Google Cloud tests, and the credential and token tests.
use super::*;
use crate::secrets::{MAX_SECRET_BYTES, Secret};
use crate::{BoxFuture, ErrorKind, Http, HttpMethod, HttpRequest, HttpResponse, ProviderError};
use base64::Engine as _;
use futures::executor::block_on;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rsa::sha2::Sha256;
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPrivateKey};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A throwaway 2048-bit key made for these tests, as its primes, so no key file is committed.
const P: [&str; 3] = [
    "ede1a911977e3231104668d124a424db77b59f850d9a211bc24d7f9dd26f3afa0c54c11b328ba3174adabbf71b7e3dff",
    "b6ad99c03bcc0f02c9604e77e50cdf7c88722a25cb70bf7b042af5f9a3b6b3bc9c9eebbe9d81a68521fc8567ec2cfdc3",
    "85d4edd5ffe4868a1915daa962c01cf9debc76692a372128794560c1461ae3a7",
];
const Q: [&str; 3] = [
    "d0b77b1ec12f5f42cfe3001a758503ed0778246afcd1956073f75e6844d519369660762e5a7036271170b3b1d2bd1fb3",
    "9e1dbdcb5d2c004bb56917fb5ded3ebc9891f86b0e161aa9040f7ac42e93e208a7f9a0e2e3c4ca9c787436b9e770954d",
    "87eaf78ddf780eae481eaa5a22c7b6a0cf8b7bf4eda17f32680190a435da22ed",
];
pub(super) const EMAIL: &str = "fixture@nodecraft-test.iam.gserviceaccount.com";
pub(super) const PROJECT: &str = "nodecraft-test";
const KEY_ID: &str = "0123456789abcdef0123456789abcdef01234567";
pub(super) const NOW: u64 = 1_790_000_000;

fn fixture_key() -> RsaPrivateKey {
    let prime = |chunks: [&str; 3]| BigUint::parse_bytes(chunks.concat().as_bytes(), 16).unwrap();
    RsaPrivateKey::from_p_q(prime(P), prime(Q), BigUint::from(65_537u32)).unwrap()
}
/// A key file as Google Cloud's console downloads it.
pub(super) fn key_file() -> Value {
    json!({
        "type": "service_account",
        "project_id": PROJECT,
        "private_key_id": KEY_ID,
        "private_key": fixture_key().to_pkcs8_pem(LineEnding::LF).unwrap().to_string(),
        "client_email": EMAIL,
        "client_id": "123456789",
        "auth_uri": "https://accounts.google.com/o/oauth2/auth",
        "token_uri": TOKEN_URI,
        "universe_domain": "googleapis.com",
    })
}
/// The keychain value for the fixture account.
pub(super) fn stored() -> Secret {
    compact(&serde_json::to_string_pretty(&key_file()).unwrap()).unwrap()
}
pub(super) fn fixed_clock(now: u64) -> Clock {
    Arc::new(move || Some(now))
}

/// The fixture transport: replies in order, and which of the two send paths each request took.
#[derive(Default)]
pub(super) struct Transport {
    requests: Mutex<Vec<(bool, HttpRequest)>>,
    replies: Mutex<VecDeque<Result<HttpResponse, ProviderError>>>,
}
impl Transport {
    pub(super) fn new(replies: Vec<Result<HttpResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(vec![]), replies: Mutex::new(replies.into()) })
    }
    pub(super) fn requests(&self) -> Vec<(bool, HttpRequest)> {
        self.requests.lock().unwrap().clone()
    }
    fn reply(&self, redirects: bool, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.requests.lock().unwrap().push((redirects, r));
        let reply = self.replies.lock().unwrap().pop_front().unwrap_or_else(|| Err(ProviderError::new(ErrorKind::Network, "no fixture reply left")));
        Box::pin(async { reply })
    }
}
impl Http for Transport {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.reply(true, r)
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.reply(false, r)
    }
}
pub(super) fn ok(v: Value) -> Result<HttpResponse, ProviderError> {
    Ok(HttpResponse { status: 200, headers: vec![], body: serde_json::to_vec(&v).unwrap() })
}
pub(super) fn granted(token: &str) -> Result<HttpResponse, ProviderError> {
    ok(json!({"access_token": token, "expires_in": 3599, "token_type": "Bearer"}))
}

#[test]
fn a_key_file_is_stored_compactly_and_signs_a_verifiable_token_request() {
    let secret = stored();
    let value = secret.expose();
    assert!(value.starts_with(&format!("gsa1:{EMAIL}:{PROJECT}:{KEY_ID}:")), "{}", value.len());
    assert!(value.len() <= MAX_SECRET_BYTES && value.bytes().all(|b| b.is_ascii_graphic()));
    assert!(!value.contains("PRIVATE KEY"));
    // The compact form is accepted again as it is.
    assert_eq!(compact(value).unwrap().expose(), value);

    let account = ServiceAccount::from_secret(&secret).unwrap();
    assert_eq!((account.email(), account.project()), (EMAIL, PROJECT));
    assert!(!format!("{account:?}").contains(&value[value.len() - 40..]), "Debug never shows the key");
    let jwt = account.assertion(NOW).unwrap();
    let [header, claims, signature] = jwt.split('.').collect::<Vec<_>>()[..] else { panic!("three parts") };
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let decode = |s: &str| serde_json::from_slice::<Value>(&b64.decode(s).unwrap()).unwrap();
    assert_eq!(decode(header), json!({"alg": "RS256", "typ": "JWT", "kid": KEY_ID}));
    assert_eq!(
        decode(claims),
        json!({"iss": EMAIL, "scope": "https://www.googleapis.com/auth/cloud-platform", "aud": TOKEN_URI, "iat": NOW, "exp": NOW + 3600})
    );
    let verifier = VerifyingKey::<Sha256>::new(fixture_key().to_public_key());
    let signature = Signature::try_from(b64.decode(signature).unwrap().as_slice()).unwrap();
    verifier.verify(format!("{header}.{claims}").as_bytes(), &signature).expect("Google can verify the signature with the account's public key");
}

#[test]
fn bad_key_files_are_refused_without_quoting_them() {
    let refused = |file: Value| {
        let text = file.to_string();
        let message = compact(&text).unwrap_err().to_string();
        assert!(!message.contains("BEGIN") && !message.contains(EMAIL), "{message}");
        message
    };
    let with = |name: &str, value: Value| {
        let mut file = key_file();
        file[name] = value;
        refused(file)
    };
    assert!(with("type", json!("authorized_user")).contains("service account"));
    assert!(with("token_uri", json!("https://attacker.example/token")).contains("token endpoint"));
    assert!(with("client_email", json!("not an address")).contains("client_email"));
    assert!(with("project_id", json!("example.com:legacy")).contains("domain-scoped"));
    assert!(with("private_key_id", json!("zz")).contains("private_key_id"));
    assert!(with("private_key", json!("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n")).contains("can't be read"));
    let small = rsa::RsaPrivateKey::from_p_q(BigUint::from(61u32), BigUint::from(53u32), BigUint::from(17u32));
    assert!(small.is_err() || with("private_key", json!(small.unwrap().to_pkcs8_pem(LineEnding::LF).unwrap().to_string())).contains("RSA key"));
    let mut missing = key_file();
    missing.as_object_mut().unwrap().remove("private_key");
    assert!(refused(missing).contains("key file"));
    for junk in ["", "sk-not-json", "{", "[]", "null", &"x".repeat(20_000), "gsa1:", "gsa1:a@b:p:00:AA:AA", "gsa1:a@b:p:00:!!:AA:extra"] {
        assert!(compact(junk).is_err(), "{junk:.20}");
        if let Ok(secret) = Secret::new(junk) {
            assert_eq!(ServiceAccount::from_secret(&secret).unwrap_err().kind, ErrorKind::Auth);
        }
    }
}

#[test]
fn a_token_is_requested_once_then_reused_until_near_expiry() {
    let t = Transport::new(vec![granted("ya29.first"), granted("ya29.second")]);
    let now = Arc::new(Mutex::new(NOW));
    let clock_now = now.clone();
    let tokens = Tokens::new(ServiceAccount::from_secret(&stored()).unwrap(), t.clone(), Arc::new(move || Some(*clock_now.lock().unwrap())));
    assert_eq!(block_on(tokens.bearer()).unwrap(), "ya29.first");
    *now.lock().unwrap() = NOW + 3000;
    assert_eq!(block_on(tokens.bearer()).unwrap(), "ya29.first", "still well inside its hour");
    *now.lock().unwrap() = NOW + 3599 - 299;
    assert_eq!(block_on(tokens.bearer()).unwrap(), "ya29.second", "replaced five minutes before it expires");
    let requests = t.requests();
    assert_eq!(requests.len(), 2);
    let (redirects, request) = &requests[0];
    assert!(!redirects, "the token request never follows a redirect");
    assert_eq!((request.method, request.url.as_str()), (HttpMethod::Post, TOKEN_URI));
    assert!(request.headers.iter().any(|(n, v)| n == "Content-Type" && v == "application/x-www-form-urlencoded"));
    let body = String::from_utf8(request.body.clone()).unwrap();
    let assertion = body.strip_prefix("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=").unwrap();
    assert_eq!(assertion.split('.').count(), 3);
    tokens.forget();
    assert!(block_on(tokens.bearer()).is_err(), "a forgotten token is requested again (no reply left)");
}

#[test]
fn refused_and_odd_token_replies_are_errors_a_person_can_act_on() {
    let reply = |status: u16, body: &str| Ok(HttpResponse { status, headers: vec![], body: body.as_bytes().to_vec() });
    let cases = [
        (reply(400, r#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#), ErrorKind::Auth, "clock"),
        (reply(401, r#"{"error":"invalid_client"}"#), ErrorKind::Auth, "still exists"),
        (reply(429, "slow down"), ErrorKind::RateLimited, "rate limiting"),
        (reply(503, ""), ErrorKind::Server, "failed"),
        (reply(302, ""), ErrorKind::Auth, "refused"),
        (ok(json!({"access_token": "ya29.x", "expires_in": 3599, "token_type": "MAC"})), ErrorKind::Malformed, "no usable token"),
        (ok(json!({"access_token": "has space", "expires_in": 3599, "token_type": "Bearer"})), ErrorKind::Malformed, "no usable token"),
        (ok(json!({"access_token": "x".repeat(5000), "expires_in": 3599, "token_type": "Bearer"})), ErrorKind::Malformed, "no usable token"),
        (ok(json!({"access_token": "ya29.x", "expires_in": -1, "token_type": "Bearer"})), ErrorKind::Malformed, "can't be read"),
        (Err(ProviderError::new(ErrorKind::Network, "dns")), ErrorKind::Network, "couldn't be reached"),
    ];
    for (reply, kind, words) in cases {
        let tokens = Tokens::new(ServiceAccount::from_secret(&stored()).unwrap(), Transport::new(vec![reply]), fixed_clock(NOW));
        let error = block_on(tokens.bearer()).unwrap_err();
        assert_eq!(error.kind, kind, "{error:?}");
        assert!(error.message.contains(words), "{}", error.message);
        assert!(!error.message.contains("Invalid JWT"), "the reply isn't quoted");
    }
    // A token too short-lived to keep serves its request only.
    let t = Transport::new(vec![ok(json!({"access_token": "ya29.brief", "expires_in": 60, "token_type": "bearer"})), granted("ya29.next")]);
    let tokens = Tokens::new(ServiceAccount::from_secret(&stored()).unwrap(), t, fixed_clock(NOW));
    assert_eq!(block_on(tokens.bearer()).unwrap(), "ya29.brief");
    assert_eq!(block_on(tokens.bearer()).unwrap(), "ya29.next");
    // No clock, no token request.
    let t = Transport::new(vec![]);
    let tokens = Tokens::new(ServiceAccount::from_secret(&stored()).unwrap(), t.clone(), Arc::new(|| None));
    assert_eq!(block_on(tokens.bearer()).unwrap_err().kind, ErrorKind::Auth);
    assert!(t.requests().is_empty());
    assert!(system_clock()().is_some_and(|s| s > 1_700_000_000));
}
