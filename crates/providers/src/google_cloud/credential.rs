//! A service account's key, kept compact in the keychain, and the signed JWT that asks Google for
//! an access token.
//!
//! A key file is about 2.3 KB of JSON, more than a keychain entry holds everywhere (Windows keeps
//! about 1280 characters). Only what signing needs is stored: the account's address, its project,
//! the key id and the two primes of the RSA key, as one line of visible ASCII
//! (`gsa1:<email>:<project>:<key id>:<p>:<q>`, the primes in unpadded base64url). The modulus and
//! private exponent are rebuilt from the primes when the adapter is made.
use crate::secrets::{MAX_SECRET_BYTES, Secret, SecretError};
use crate::{ErrorKind, ProviderError};
use base64::Engine as _;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BigUint, RsaPrivateKey};
use serde::Deserialize;

/// The keychain account holding the service account.
pub const ACCOUNT: &str = "google-cloud";
/// Google's OAuth token endpoint: the JWT's audience, and the only place it is sent.
pub const TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const FORMAT: &str = "gsa1";
/// A key file is about 2.3 KB; anything far larger isn't one.
const MAX_KEY_FILE_BYTES: usize = 16 * 1024;
const EXPONENT: u32 = 65_537;
/// Google issues 2048-bit keys; uploaded ones may be larger. The compact form of a 4096-bit key
/// fits a keychain entry unless the account's address is unusually long; that is refused.
const MIN_BITS: usize = 2048;
const MAX_BITS: usize = 4096;
/// A requested token lives an hour, the most Google grants.
const LIFETIME_S: u64 = 3600;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}
fn invalid(message: &'static str) -> SecretError {
    SecretError::Invalid(message.into())
}

/// The fields of a key file this reads; the others (`auth_uri`, `client_id`, …) are ignored.
#[derive(Deserialize)]
struct KeyFile {
    #[serde(rename = "type")]
    kind: String,
    project_id: String,
    private_key_id: String,
    private_key: String,
    client_email: String,
    token_uri: Option<String>,
}

fn email_ok(s: &str) -> bool {
    (3..=254).contains(&s.len()) && s.bytes().filter(|b| *b == b'@').count() == 1 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"@.-_+".contains(&b))
}
/// Project ids are lowercase letters, digits and hyphens. Legacy domain-scoped ids
/// (`example.com:project`) aren't supported: they would need escaping in every URL.
fn project_ok(s: &str) -> bool {
    (1..=63).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn key_id_ok(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn shape_ok(key: &RsaPrivateKey) -> bool {
    let bits = key.size().saturating_mul(8);
    (MIN_BITS..=MAX_BITS).contains(&bits) && key.e() == &BigUint::from(EXPONENT) && key.primes().len() == 2
}

/// A pasted service-account key file, made compact for the keychain. A value already in the
/// compact form is checked and kept. Messages never quote the file.
pub fn compact(value: &str) -> Result<Secret, SecretError> {
    let value = value.trim();
    if value.starts_with(FORMAT) {
        ServiceAccount::from_compact(value).map_err(|_| invalid("the stored form of the Google Cloud key is damaged; paste the key file again"))?;
        return Secret::new(value);
    }
    if value.len() > MAX_KEY_FILE_BYTES {
        return Err(invalid("that is too large to be a Google Cloud service-account key file"));
    }
    let file: KeyFile = serde_json::from_str(value).map_err(|_| invalid("paste the whole service-account key file (JSON) from Google Cloud"))?;
    if file.kind != "service_account" {
        return Err(invalid("that key file isn't for a service account"));
    }
    if file.token_uri.as_deref().is_some_and(|uri| uri != TOKEN_URI) {
        return Err(invalid("that key file names a token endpoint other than Google's"));
    }
    if !email_ok(&file.client_email) {
        return Err(invalid("the key file's client_email isn't a service-account address"));
    }
    if !project_ok(&file.project_id) {
        return Err(invalid("the key file's project id isn't supported (domain-scoped projects aren't)"));
    }
    if !key_id_ok(&file.private_key_id) {
        return Err(invalid("the key file's private_key_id isn't a key id"));
    }
    let key = RsaPrivateKey::from_pkcs8_pem(&file.private_key).map_err(|_| invalid("the key file's private key can't be read"))?;
    if !shape_ok(&key) {
        return Err(invalid("the key file's private key isn't a 2048 to 4096-bit RSA key"));
    }
    let [p, q] = key.primes() else {
        return Err(invalid("the key file's private key isn't a two-prime RSA key"));
    };
    let compact = format!(
        "{FORMAT}:{}:{}:{}:{}:{}",
        file.client_email,
        file.project_id,
        file.private_key_id.to_ascii_lowercase(),
        b64().encode(p.to_bytes_be()),
        b64().encode(q.to_bytes_be())
    );
    if compact.len() > MAX_SECRET_BYTES {
        return Err(invalid("that service account's address and key are too long to store together; use a 2048-bit key"));
    }
    Secret::new(&compact)
}

/// A service account that can sign its token requests. It never prints its key.
pub struct ServiceAccount {
    email: String,
    project: String,
    key_id: String,
    signer: SigningKey<Sha256>,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount").field("email", &self.email).field("project", &self.project).finish_non_exhaustive()
    }
}

impl ServiceAccount {
    /// The account stored in the keychain.
    pub fn from_secret(secret: &Secret) -> Result<Self, ProviderError> {
        Self::from_compact(secret.expose())
            .map_err(|_| ProviderError::new(ErrorKind::Auth, "the stored Google Cloud key can't be used; paste the service-account key file again"))
    }

    fn from_compact(value: &str) -> Result<Self, ()> {
        let mut parts = value.split(':');
        let (Some(FORMAT), Some(email), Some(project), Some(key_id), Some(p), Some(q), None) =
            (parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(());
        };
        if !email_ok(email) || !project_ok(project) || !key_id_ok(key_id) || p.len() > 1024 || q.len() > 1024 {
            return Err(());
        }
        let prime = |s: &str| b64().decode(s).map(|bytes| BigUint::from_bytes_be(&bytes)).map_err(|_| ());
        let key = RsaPrivateKey::from_p_q(prime(p)?, prime(q)?, BigUint::from(EXPONENT)).map_err(|_| ())?;
        if !shape_ok(&key) {
            return Err(());
        }
        Ok(Self { email: email.to_owned(), project: project.to_owned(), key_id: key_id.to_owned(), signer: SigningKey::new(key) })
    }

    pub fn email(&self) -> &str {
        &self.email
    }
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The signed JWT that asks [`TOKEN_URI`] for an hour's access token, issued at `now_s`
    /// (seconds since the Unix epoch).
    pub fn assertion(&self, now_s: u64) -> Result<String, ProviderError> {
        let header = serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": self.key_id});
        let claims = serde_json::json!({
            "iss": self.email,
            "scope": SCOPE,
            "aud": TOKEN_URI,
            "iat": now_s,
            "exp": now_s.saturating_add(LIFETIME_S),
        });
        let input = format!("{}.{}", b64().encode(header.to_string()), b64().encode(claims.to_string()));
        // PKCS#1 v1.5 signing is deterministic. It runs at most once an hour, on the person's own
        // machine, so the unblinded signature's timing reveals nothing anyone can measure.
        let signature =
            self.signer.try_sign(input.as_bytes()).map_err(|_| ProviderError::new(ErrorKind::Auth, "the Google Cloud key couldn't sign the token request"))?;
        Ok(format!("{input}.{}", b64().encode(signature.to_bytes())))
    }
}
