## 2026-10-17 · MCP bridge (#116)

- **The bridge is the control channel, not a second server.** `nodecraft-cli mcp --bridge` keeps
  the MCP protocol in the CLI and forwards `command_list`, `command_run` and `canvas_inspect` to
  the app's control channel (`control::Client`). The app's session, undo and command policy
  apply unchanged, and there is one authenticated, bounded way into a running app. `ids_new`
  stays local; `screenshot` exists only when bridged.
- **Reconnect, never resend.** The app closes connections idle for 10 minutes, so the client
  reconnects before a call after 9 minutes, re-reading the token file. After a lost or
  out-of-step reply, the next call reconnects. The call that failed is reported, never resent,
  so a command can't run twice.

