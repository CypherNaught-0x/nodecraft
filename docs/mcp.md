# MCP server

`nodecraft-cli mcp` serves NodeCraft to MCP clients (Claude Code, Claude Desktop, other agents)
over stdio: newline-delimited JSON-RPC on stdin and stdout, with diagnostics on stderr. It speaks
MCP protocol revisions 2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05.

```sh
claude mcp add nodecraft -- nodecraft-cli mcp      # Claude Code
```

For other clients, configure a stdio server that runs `nodecraft-cli` with the argument `mcp`.

The server starts with a new, empty project with one canvas. Opening and saving project files
arrives with the project format (#54), confined to the read/write roots you allow. Bridging to a
running app, streamable HTTP and the in-app server setting are still to come (#116).

## Tools

The tools are generic, so every engine command is available without a second list to maintain.
They go through the same `Session::dispatch` as the app's menus and shortcuts.

| Tool | What it does |
|---|---|
| `command_list` | The commands you can run: id, label and parameter schema. Commands without parameters also say whether they can run right now (`ready`, with the reason when not). `query` filters by id or label. |
| `command_run` | Runs one command with `params`. Document edits are undoable (`edit.undo`). |
| `canvas_inspect` | Without arguments: the project and its canvases with counts. With `canvas`: a page of its nodes, sorted by id, from `offset`, with the edges into them. A page holds at most `limit` nodes (up to 500) and is also cut short at about 512 KiB; continue from `next_offset`. Nodes over 64 KiB are summarised in pages; read one in full with `canvas` and `node`. |
| `ids_new` | Up to 100 fresh UUIDs for commands that take a caller-chosen id, such as `node.add`. |

A command that fails comes back as a tool result with `isError: true` and a message the model can
act on. Protocol mistakes (an unknown tool, malformed arguments) are JSON-RPC errors.

**Every command is classified.** Commands in `mcp::AGENT_COMMANDS` are listed and run. Commands
in `mcp::HUMAN_ONLY` are neither: `keys.set` would pass an API key through the agent's context,
and `keys.remove` deletes a key with no undo. A new command stays hidden until it is added to one
of the lists, and a test fails until then.

**Keychain prompts:** `keys.status` reads the OS keychain. The first time `nodecraft-cli` reads a
key the app stored, macOS can ask whether to allow it. Messages are handled one at a time, so the
server waits until the prompt is answered.

**Limits:** messages are at most 2 MiB (the engine accepts at most 1 MiB of command parameters).
A longer line is skipped with an error reply instead of being buffered. Messages must be UTF-8
JSON-RPC 2.0. Batches aren't accepted, since MCP removed them in 2025-06-18.

Paid operations (running generations) aren't exposed yet. When they arrive, they will show their
cost first and respect the session's budget (plan §4.8, #207).
