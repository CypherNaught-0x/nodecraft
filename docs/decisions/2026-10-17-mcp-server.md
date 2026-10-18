## 2026-10-17 · MCP server (#116)

- **Generic tools, not one per command.** `command_list` and `command_run` follow the engine
  registry, so a new command is available to agents with no MCP change, and runs through the
  same `Session::dispatch` (validation, undo, events) as the UI. Techniques will get their own
  tools, since they are the user's published units.
- **Sans-IO server.** `automation::mcp::McpServer::handle` maps one JSON-RPC message to its reply;
  `serve` adds the stdio framing. The bridge and HTTP transports reuse the same handler.
- **Every command is classified for automation**: `AGENT_COMMANDS` may be listed and run, and
  `HUMAN_ONLY` never is. `HUMAN_ONLY` holds commands that take a secret (`keys.set`) or destroy
  something with no undo (`keys.remove`). An unclassified command is hidden, and a test fails
  until it is classified, so adding a command forces the decision.
- **Replies are size-capped, not just counted**: `canvas_inspect` pages stop at about 512 KiB,
  and large nodes are summarised and fetched one at a time.
- **Ids come from the host.** Commands take caller-chosen ids, so `ids_new` hands out UUIDs from a
  host-supplied random source; the CLI seeds it from the standard library's per-process random
  keys.

