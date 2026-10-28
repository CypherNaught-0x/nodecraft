## 2026-10-28 · MCP over Streamable HTTP and the app's built-in server (#116)

The MCP server gained the Streamable HTTP transport (`nodecraft-cli mcp --listen`) and a server
built into the desktop app (Automation › Enable MCP server). Choices others will build on:

- **Hand-written HTTP/1.1, no new dependency.** The transport (`automation::mcp::http`) is a small
  std-only server: one endpoint, `POST` of one JSON-RPC message with `Content-Length`, JSON replies.
  It never streams; `GET` is `405`, which the specification allows, so there is no SSE code to
  secure. Adding streaming later (server-initiated notifications) means answering `GET` with an
  event stream on the same endpoint, not a second listener.
- **Secure by default, not configurable down.** Loopback addresses only (others are refused at
  bind), a required bearer token, and `Host`/`Origin` checks against DNS rebinding before the body
  is read. There is deliberately no flag to listen on a network address; remote access, if ever
  wanted, needs its own decision (TLS, accounts).
- **One server, many sessions.** Session ids are protocol bookkeeping; every session shares the one
  `McpServer` (one project or one bridged app), and messages are handled one at a time on the
  thread that owns it, as stdio does. A message still queued when its connection gives up is
  dropped, never run late, so a client retry can't run a command twice.
- **The app's server is a bridge without a socket.** It runs `McpServer::in_app` over
  `control::LocalClient`, which queues the same `control::Call`s a control connection does; the
  window answers them through the same `CallPump`. So classification, live-only commands, file
  roots, quotes, budgets and the panic guard are the control channel's, with no second dispatch
  path.
- **Token in the keychain, never in a command.** The app keeps its token in the keychain account
  `nodecraft-mcp-server`, so client configs survive restarts; `automation.mcp_server_status` never
  returns it, and the panel shows it masked with copy buttons. The CLI's token is
  `NODECRAFT_MCP_TOKEN` or a fresh one shown once on stderr, and is never stored.
- **Turning the server on is human-only** (`automation.mcp_server_set`); reading its status is not.
- **Default ports**: 41116 for the app, 41117 for the CLI, so both can run at once.

Not built: one tool per published Technique. The generic `command_run` already reaches
`technique.*` and `app.*` commands, and the issue thread never settled whether separate tools are
still wanted; that is left for a follow-up decision.
