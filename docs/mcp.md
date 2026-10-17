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
| `canvas_inspect` | Without `canvas`: the project and a page of canvas summaries sorted by id, with counts, `total`, `next_offset` and `revision`; `offset` and `limit` (1–500, default 100) select the page. Overview structured JSON, including metadata, is at most 512 KiB. With `canvas`: a page of its nodes, sorted by id, from `offset`, with the edges into them. A page holds at most `limit` nodes (up to 500) and is also cut short at about 512 KiB; continue from `next_offset`. Nodes over 64 KiB are summarised in pages; read one in full with `canvas` and `node`, which also pages all of that node's edges (`edge_offset`, `next_edge_offset`). A page that couldn't fit all of a node's incoming edges sets `edges_truncated` and names the node in `edges_continue`. |
| `ids_new` | Up to 100 fresh UUIDs for commands that take a caller-chosen id, such as `node.add`. |

Overview pagination ends with `next_offset: null`; offsets at or past the end return an empty
`canvases` array and the same `total`. Invalid overview offsets/limits are refused, not clamped;
`node` requires its `canvas`, and an overview cannot take `edge_offset`. Compare `revision` across
pages and restart after an edit: pages do not reserve a snapshot. Small overviews retain their
`project` and `canvases` fields.

The overview's 512 KiB bound counts encoded structured page JSON, including escaped project and
canvas titles, envelope and continuation metadata, before text is copied. It is **not** a final
wire limit: MCP also carries the page as text, whose quotes/backslashes escape again. A wire reply
can exceed 1 MiB; the JSON-RPC request id adds its own bytes. Node/edge pages retain the approximate
payload policy above. Full-node inspection retains the graph's 8 MiB node ceiling, independently
of page budgets; this change does not impose a transport-wide response cap.

`canvas_inspect` returns stored node titles. An empty title selects the automatic title derived
from the prompt or kind label in the app; it does not indicate missing node data. Use `node.title`
to rename a node (empty restores automatic naming), and `node.param_set` to edit a schema
parameter (null removes its override and restores the default).

A command that fails comes back as a tool result with `isError: true` and a message the model can
act on. Protocol mistakes (an unknown tool, malformed arguments) are JSON-RPC errors.

**Every command is classified.** Commands in `mcp::AGENT_COMMANDS` are listed and run. Commands
in `mcp::HUMAN_ONLY` are neither: `keys.set` would pass an API key through the agent's context,
and `keys.remove` deletes a key with no undo. A new command stays hidden until it is added to one
of the lists, and a test fails until then.

**Keychain prompts:** `keys.status` reads the OS keychain. The first time `nodecraft-cli` reads a
key the app stored, macOS can ask whether to allow it. Messages are handled one at a time, so the
server waits until the prompt is answered.

**Limits:** messages are at most 4 MiB. The engine accepts at most 1 MiB of decoded command parameters, and clients may escape non-ASCII text.
A longer line is skipped with an error reply instead of being buffered. Messages must be UTF-8
JSON-RPC 2.0. Batches aren't accepted, since MCP removed them in 2025-06-18.

Navigation commands (`canvas.fit_all`, `canvas.fit_selection`, `canvas.minimap_toggle`,
`canvas.pan_to`) are discoverable but require a scoped live canvas host. The standalone stdio
session reports them unavailable and returns Unsupported when run; they never open a window
or change the document.

Paid operations (running generations) aren't exposed yet. When they arrive, they will show their
cost first and respect the session's budget (plan §4.8, #207).
