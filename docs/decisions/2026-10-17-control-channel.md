## 2026-10-17 · Control channel (#115)

- **Local TCP with a token file.** `nodecraft --control` binds 127.0.0.1 only, on a random port.
  It writes `control.json` (port, a 32-byte random token, pid) into the settings folder, readable
  by the owner only. Every connection starts with `hello` and the token, compared in constant
  time, or it is closed.
- **The UI thread owns the session.** Connection threads only parse and authenticate. Requests
  reach the UI thread through a bounded queue and are answered against the session the window
  shows, so changes appear live and go through the same undo path. The app crate holds the glue
  (`apps/nodecraft/src/control.rs`), since `ui-egui` and `automation` share a layer.
- **One automation policy.** The control channel reuses MCP's command listing, running and
  inspection (`AGENT_COMMANDS`/`HUMAN_ONLY`, bounded pages), so it can't do more than an agent
  can.
- **Screenshots** come from egui's viewport screenshot, encoded as PNG by `media::encode_png`. They
  are encoded off the UI thread, and they fail when the window doesn't draw.
- **Hostile local clients.** The handshake is short, everything but `hello` is refused before
  authentication, and read and write timeouts apply. Requests the client gave up on are never
  run late. The token file is written through a new file and a rename, never through a link, and
  is refused in a shared folder.
- **Stdio is deferred.** A GUI app has no useful stdin, and headless stdio automation already
  exists as `nodecraft-cli mcp`. A stdio control mode would serve the same protocol over the CLI
  later.
