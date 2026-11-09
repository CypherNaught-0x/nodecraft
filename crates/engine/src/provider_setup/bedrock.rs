//! How the `bedrock` adapter authenticates (#1163): a Bedrock API key when one is stored, else
//! standard AWS credentials signed with SigV4. See
//! `docs/decisions/2026-11-08-bedrock-sigv4-aws-credentials.md`.
//!
//! Keychain reads can prompt, so the AWS accounts are read only when there is no API key, and
//! the temporary-credential ones only once an access key ID and secret are found.

use nodecraft_providers::aws::{self, AwsCredentials};
use nodecraft_providers::llm::{
    ApiKey, BEDROCK, BEDROCK_AWS_ACCESS_KEY_ID, BEDROCK_AWS_EXPIRATION, BEDROCK_AWS_SECRET_ACCESS_KEY, BEDROCK_AWS_SESSION_TOKEN, BedrockAuth,
};

use super::{Readiness, read_key};
use crate::secrets::{Account, Secret, SecretStore};

fn account(name: &str) -> Result<Account, Readiness> {
    Account::new(name).map_err(|_| Readiness::Failed("its keychain account name is invalid".into()))
}

fn stored(keys: &dyn SecretStore, name: &str) -> Result<Option<Secret>, Readiness> {
    read_key(keys, &account(name)?)
}

/// The Bedrock credentials stored in `keys`, as of Unix second `now` (`None`: the clock is unset,
/// and an expiry can't be checked here; the signer refuses then).
pub(super) fn auth(keys: &dyn SecretStore, now: Option<u64>) -> Result<BedrockAuth, Readiness> {
    if let Some(key) = stored(keys, BEDROCK)? {
        return Ok(BedrockAuth::ApiKey(ApiKey::from(&key)));
    }
    let (access, secret) = match (stored(keys, BEDROCK_AWS_ACCESS_KEY_ID)?, stored(keys, BEDROCK_AWS_SECRET_ACCESS_KEY)?) {
        (Some(access), Some(secret)) => (access, secret),
        (None, None) => return Err(Readiness::NeedsKey { account: BEDROCK.into() }),
        (Some(_), None) => return Err(Readiness::NeedsKey { account: BEDROCK_AWS_SECRET_ACCESS_KEY.into() }),
        (None, Some(_)) => return Err(Readiness::NeedsKey { account: BEDROCK_AWS_ACCESS_KEY_ID.into() }),
    };
    let token = stored(keys, BEDROCK_AWS_SESSION_TOKEN)?;
    let mut credentials =
        AwsCredentials::from_secrets(&access, &secret, token.as_ref()).map_err(|error| Readiness::NotSetUp(format!("{} (API keys)", error.message)))?;
    if let Some(expiry) = stored(keys, BEDROCK_AWS_EXPIRATION)? {
        let at = aws::parse_expiry(expiry.expose()).map_err(|error| Readiness::NotSetUp(format!("{BEDROCK_AWS_EXPIRATION}: {}", error.message)))?;
        credentials = credentials.with_expiry(at);
        if let Some(now) = now {
            credentials.check_fresh(now).map_err(|error| Readiness::NotSetUp(error.message))?;
        }
    }
    Ok(BedrockAuth::Aws(credentials))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;

    // AWS's published example credentials, never real ones.
    const ACCESS: &str = "AKIDEXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    fn set(keys: &MemoryStore, name: &str, value: &str) {
        keys.set(&Account::new(name).unwrap(), &Secret::new(value).unwrap()).unwrap();
    }

    #[test]
    fn an_api_key_wins_and_aws_credentials_need_both_halves() {
        let keys = MemoryStore::default();
        assert_eq!(auth(&keys, Some(0)).unwrap_err(), Readiness::NeedsKey { account: BEDROCK.into() });
        set(&keys, BEDROCK_AWS_ACCESS_KEY_ID, ACCESS);
        assert_eq!(auth(&keys, Some(0)).unwrap_err(), Readiness::NeedsKey { account: BEDROCK_AWS_SECRET_ACCESS_KEY.into() });
        set(&keys, BEDROCK_AWS_SECRET_ACCESS_KEY, SECRET);
        let BedrockAuth::Aws(credentials) = auth(&keys, Some(0)).unwrap() else { panic!("AWS credentials") };
        assert!(!credentials.has_session_token() && credentials.expires_at().is_none());
        set(&keys, BEDROCK, "bedrock-public-fixture-key");
        assert!(matches!(auth(&keys, Some(0)).unwrap(), BedrockAuth::ApiKey(_)), "the API key wins");
        let keys = MemoryStore::default();
        set(&keys, BEDROCK_AWS_SECRET_ACCESS_KEY, SECRET);
        assert_eq!(auth(&keys, Some(0)).unwrap_err(), Readiness::NeedsKey { account: BEDROCK_AWS_ACCESS_KEY_ID.into() });
    }

    #[test]
    fn temporary_credentials_carry_their_token_and_expiry() {
        let keys = MemoryStore::default();
        set(&keys, BEDROCK_AWS_ACCESS_KEY_ID, ACCESS);
        set(&keys, BEDROCK_AWS_SECRET_ACCESS_KEY, SECRET);
        set(&keys, BEDROCK_AWS_SESSION_TOKEN, "public-fixture-session-token");
        set(&keys, BEDROCK_AWS_EXPIRATION, "2015-08-30T12:36:00Z");
        let before = 1_440_938_160 - 3_600;
        let BedrockAuth::Aws(credentials) = auth(&keys, Some(before)).unwrap() else { panic!("AWS credentials") };
        assert!(credentials.has_session_token());
        assert_eq!(credentials.expires_at(), Some(1_440_938_160));
        // Past it, nothing is built, and the message says what to do without quoting a key.
        let Err(Readiness::NotSetUp(why)) = auth(&keys, Some(1_440_938_160)) else { panic!("expired") };
        assert!(why.contains("expired") && !why.contains(SECRET) && !why.contains("public-fixture-session-token"), "{why}");
        // With no clock the signer checks instead.
        assert!(auth(&keys, None).is_ok());
        set(&keys, BEDROCK_AWS_EXPIRATION, "tomorrow");
        let Err(Readiness::NotSetUp(why)) = auth(&keys, Some(before)) else { panic!("unreadable expiry") };
        assert!(why.contains(BEDROCK_AWS_EXPIRATION) && why.contains("RFC 3339"), "{why}");
        set(&keys, BEDROCK_AWS_ACCESS_KEY_ID, "not-an-access-key-id");
        let Err(Readiness::NotSetUp(why)) = auth(&keys, Some(before)) else { panic!("bad access key id") };
        assert!(!why.contains("not-an-access-key-id"), "{why}");
    }
}
