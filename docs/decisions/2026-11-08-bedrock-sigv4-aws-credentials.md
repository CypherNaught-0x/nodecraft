## 2026-11-08 · Bedrock signs with SigV4 from AWS access keys in the keychain (#1163)

Follows [Bedrock's native Converse](2026-11-08-bedrock-native-converse.md), which left SigV4 for
this follow-up, and [the Bedrock OpenAI route](2026-11-03-bedrock-openai-compatible-route.md).

- **A second credential type for `bedrock`, in four keychain accounts.** Besides the Bedrock API
  key (account `bedrock`), a person can store standard AWS credentials:
  `bedrock:aws-access-key-id`, `bedrock:aws-secret-access-key`, and, for temporary credentials,
  `bedrock:aws-session-token` and `bedrock:aws-expiration` (an RFC 3339 time such as
  `2026-11-08T20:00:00Z`, the `Expiration` that `aws sts get-session-token` and
  `aws configure export-credentials` print). They are ordinary keychain accounts, so the API keys
  panel stores, checks and removes them as it does every other key (they are offered as quick
  picks next to `bedrock`); no new settings, no new storage. The expiry isn't secret, but it
  belongs with the token it describes and must change with it, so it is kept beside it. NodeCraft
  still never reads AWS environment variables, profiles, SSO caches or instance metadata.
- **Precedence: the API key wins.** The engine's `bedrock` adapter reads `bedrock` first; when a
  key is stored it is sent as `Authorization: Bearer` exactly as before and the AWS accounts are
  not read (no extra keychain prompts). Only without one does it read the access key ID and
  secret; with both it signs with SigV4, with neither it asks for a key (`NeedsKey bedrock`), and
  with only one of them it says which one is missing. The session token and expiry are optional.
- **Expiry is checked twice, before any money is spent.** Readiness refuses credentials whose
  stored expiry has passed (or doesn't parse), and the signer refuses again when it signs, 30
  seconds early, so a long session never sends a request AWS would reject. Without a stored
  expiry, AWS's own `ExpiredToken` answer is mapped to an Authentication error that says to store
  fresh temporary credentials.
- **The signer is ours, pure Rust, over `Http`.** `nodecraft_providers::aws` has the SigV4
  algorithm (`hmac` 0.13 and `sha2` 0.11 from the workspace, `web-time` 1.1.0 for the clock in
  the browser too, all already locked) and `SigV4Http`, a transport that signs each request just
  before handing it to the real one. Wrapping the transport covers both Bedrock routes (the
  OpenAI-compatible route, which AWS documents with SigV4 too, and Converse), whole and streamed
  replies, and the runner's delivery scope, without touching the dialects. It signs only `https`
  requests to the one `bedrock-runtime.{region}.amazonaws.com` host it was made for, and a
  request it refuses to sign is reported as never sent. The AWS SDK and `aws-sigv4` were not
  used: the SDK doesn't build for wasm and the signer is about 300 lines.
- **What is signed.** `host`, `x-amz-date`, `x-amz-content-sha256` (the body's SHA-256, which
  Bedrock accepts), `x-amz-security-token` when there is a session token, and `content-type`.
  Other headers are left unsigned, since a transport may rewrite them.
- **The canonical URI encodes the path as sent, again.** For services other than S3, AWS's
  signers percent-encode each segment of the already-encoded request path a second time
  (botocore's `SigV4Auth._normalize_url_path`, aws-sdk-rust's `PercentEncodingMode::Double`). A
  model id such as `us.anthropic.claude-haiku-4-5-20251001-v1:0` is sent as `…-v1%3A0` and
  signed as `…-v1%253A0`; an ARN's `:` and `/` are sent as `%3A`/`%2F` and signed as
  `%253A`/`%252F`. Paths aren't normalised (Bedrock's never have empty or dot segments).
- **Test vectors.** The signer passes the cases of AWS's SigV4 test suite as published in
  `awslabs/aws-c-auth` (`tests/aws-signing-test-suite/v4`, Apache-2.0), kept as fixtures with
  their source in `ATTRIBUTION.md`. They use AWS's public example credentials (`AKIDEXAMPLE`),
  never real ones.
- **Secrets never show.** `AwsCredentials` and `SigV4Http` print nothing of the keys; the
  `Authorization` and `x-amz-security-token` headers are secret headers (never followed across a
  redirect to another origin); an error reply that quotes the session token has it removed before
  any error is built; and signature, skew and expired-token errors carry NodeCraft's own words,
  not AWS's message (which can quote the canonical request). `RequestTimeTooSkewed` and a
  "Signature expired" refusal say to set the computer's clock (AWS accepts about 15 minutes of
  difference); `SignatureDoesNotMatch` says to check the secret matches the access key ID.
- **Same money rules.** Signing changes nothing about billing: no automatic resend of a request
  that may have been sent, prices from the catalog only.
- **Limits.** A keychain entry holds at most 1024 characters (Windows' credential limit), so a
  session token longer than that can't be stored yet; `GetSessionToken` tokens fit, some role
  sessions with many tags may not. The browser keeps the same accounts in the site's storage, as
  it does every key, and still needs a custom host for the Region.
