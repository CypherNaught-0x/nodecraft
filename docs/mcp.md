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
| `command_list` | The commands you can run: id, label, parameter schema, and whether each can run right now (with the reason when not). `query` filters by id or label. |
| `command_run` | Runs one command with `params`. Document edits are undoable (`edit.undo`). |
| `canvas_inspect` | Without `canvas`: the project and its canvases with counts. With `canvas`: its nodes (paged by `offset` and `limit`, at most 500, sorted by id) and the edges touching them. |
| `ids_new` | Up to 100 fresh UUIDs for commands that take a caller-chosen id, such as `node.add`. |

A command that fails comes back as a tool result with `isError: true` and a message the model can
act on. Protocol mistakes (an unknown tool, malformed arguments) are JSON-RPC errors.

**Commands only a person may run** (`mcp::HUMAN_ONLY`) are neither listed nor run: `keys.set`
would pass an API key through the agent's context, and `keys.remove` deletes a key with no undo.
`keys.status` is available.

**Limits:** messages are at most 16 MiB; a longer line is skipped with an error reply instead of
being buffered. JSON-RPC batches aren't accepted, since MCP removed them in 2025-06-18.

Paid operations (running generations) aren't exposed yet. When they arrive, they will show their
cost first and respect the session's budget (plan §4.8, #207).
