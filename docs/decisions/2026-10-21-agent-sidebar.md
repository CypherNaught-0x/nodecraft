## 2026-10-21 · The agent sidebar lives in the desktop app, beside the canvas (#114)

- **The app crate hosts it, not `ui-egui`.** `agent` and `automation` are L5 like `ui-egui`, so
  only `apps/nodecraft` may use all three. A `Shell` there draws the sidebar, gives the canvas the
  rest of the window, and pumps the control channel, which used to wrap the app on its own.
- **The agent's tools are the MCP server's**, run against the window's live session through the
  same `LiveCommands` the control channel uses, so its edits show at once, join undo, and pass the
  same checks (and refusals) as any other client.
- **Requests go out on one worker thread** with its own current-thread runtime. Keys are read from
  the keychain when a request is sent, never held by the panel; a reply that arrives after Cancel
  or Clear is dropped by its ticket.
- **Settings, not a screen, for now.** `[agent]` holds endpoint, model, base URL, mode, budget and
  whether the sidebar is open; hostile values are read through bounded accessors. Anthropic has a
  default model; other endpoints need `model` set, and the sidebar says so instead of guessing.
- **Esc cancels only when the sidebar has focus** (or nothing does) and no popup is open, so Esc
  keeps its canvas and dialog meanings elsewhere.
