## 2026-11-06 · The browser agent waits for a shared shell crate (#1064)

Status: proposed. Building it needs a layering change, which is the owner's to accept.

The in-canvas agent (#114) runs only in the desktop app. #1064 asks how it should run in the
browser build, with the same MCP tools, the same impact checks and the browser's key storage, or
for a record of why not. This is that record: what the browser agent will be, and why it isn't
built in this change.

### What is already browser-ready

- **The model calls.** `nodecraft_providers::llm::LlmClient::chat` is async and builds for
  wasm32. It already sends Anthropic's `anthropic-dangerous-direct-browser-access` header from
  the browser (#118), and OpenAI-compatible endpoints answer pages with CORS. Ollama and LM Studio
  answer a page only once their own origin settings allow it, which the endpoint's notice must
  say.
- **The keys.** The browser keeps provider keys in the site's storage (#118); the agent's
  endpoint accounts (`anthropic`, `openai`, `openrouter`) are the same names.
- **The tool loop and impact checks.** `nodecraft-agent` builds for wasm32 (`cargo xtask wasm`
  checks it): the loop, assist/auto confirmations and the run budget have no platform code.

### What isn't

- **Where the sidebar lives.** `AgentPanel`, its toolbox and `LiveCommands` (the
  `automation::commands::CommandHost` over the live window that both the agent and the desktop
  control channel use) are in `apps/nodecraft`. The browser app (`apps/nodecraft-web`) can't use
  an app's modules, and `ui-egui` can't take them: it and `agent` are both layer 5, and a crate
  may depend only on strictly lower layers.
- **The worker.** The desktop sends each request on a thread of its own and reads the key from
  the keychain there. The browser needs the same contract (one request in flight, dropped when no
  longer wanted, the reply handed back with its ticket) on `wasm_bindgen_futures::spawn_local`,
  reading the key from site storage.

### The decision

1. Add one crate, `nodecraft-shell` at a new layer 6, between the layered crates and the apps.
   It holds `AgentPanel`, its toolbox, `LiveCommands`, and the agent worker behind a small
   `Transport` trait: a thread and the keychain on the desktop, `spawn_local` and site storage
   in the browser. Both apps depend on it; nothing below it may. `xtask/src/layers.rs` gains the
   layer, and the UI-allowed list gains the crate.
2. The browser build gets the sidebar, its Cmd/Ctrl+/ shortcut and its Settings screen (#1063).
   Assist and auto modes use the same `nodecraft-agent` confirmations and budget as the desktop,
   so the same steps wait for Approve.
3. Until then the browser says the agent needs the desktop app, as it does today.

### Why it isn't built here

A new layer changes the architecture every contributor builds on (`docs/architecture.md`,
`cargo xtask layers`). Moving the desktop control channel's command host along with the panel
touches the control channel (#116) too. Both are choices the owner should accept first, so this
change records them for review rather than making them. Once accepted, the work is: the crate and
its layer entry; moving the panel and `LiveCommands` without behaviour change, with the desktop's
existing kittests as the guard; the browser transport, with tests of the request contract on a
scripted transport; and a browser run of assist and auto modes against a local OpenAI-compatible
server.
