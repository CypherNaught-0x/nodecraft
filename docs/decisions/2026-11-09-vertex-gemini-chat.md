## 2026-11-09 · Vertex Gemini chat uses the existing service-account path (#1151)

Vertex chat uses rig's native Gemini GenerateContent wire over `RigHttp`, with a fixed
Google publisher route in the service account's own project. The account's compact key remains
in the existing `google-cloud` keychain entry. The shared signer exchanges it for a short-lived
OAuth bearer token; only `RigHttp` receives that token, as a secret header. Rig receives an
empty key. A token rejected with an authorization error is forgotten for the next explicit
request, without automatically retrying the potentially billed chat call.

Gemini's GenerateContent fields are shared by the Developer API and Vertex REST surface:
contents, systemInstruction, native function declarations and responses, candidates, and
usageMetadata. The route and authorization differ. Original parts are retained for signed tool
turns, and thinking tokens count toward output usage. NodeCraft retains its own cost and
error policy; provider responses never supply user-facing error text on this path.

The built-in catalog card uses `global`; embedding hosts can choose a validated regional
location through `LlmClient::vertex`. Express-mode API keys are excluded: this first path reuses
the project's existing service-account/IAM setup without another credential surface. No custom
URL is supported, so that credential cannot be redirected to another host.

Current Vertex pricing and live model availability were not verified in this environment.
The card therefore reports unknown cost (`by_usage`), and the live smoke test is explicitly
opt-in. Fixture tests establish the local wire contract, not service availability or browser
CORS. See [provider setup and limitations](../providers.md#vertex-ai-chat-vertex-llm-feature-1151).
