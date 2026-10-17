# Control channel

`nodecraft --control` lets another process on the same machine drive the running app: run
commands, read the project, and take screenshots of the window. The MCP bridge and test
harnesses use it.

## Connecting

The app listens on `127.0.0.1` on a random port, and writes `control.json` into its settings
folder (with owner-only permissions) holding `{"port", "token", "pid"}`. It removes the file when
it quits normally. With in-memory settings (`NODECRAFT_CONFIG_DIR=`), the same JSON is printed to
stdout instead.

Messages are newline-delimited JSON, one request and one reply per line, each with the same
`id`. A connection must begin with `hello` and the token; anything else closes it.

```text
→ {"id": 1, "method": "hello", "params": {"token": "…"}}
← {"id": 1, "result": {"app": "nodecraft", "version": "0.1.0", "protocol": 1}}
```

## Methods

| Method | Params | Result |
|---|---|---|
| `hello` | `token` | app, version, protocol |
| `commands.list` | `query` (optional) | the commands automation may run, with their parameter schemas |
| `command.run` | `id`, `params` | `{result, revision}`; runs through the session the window shows, so the change appears live and is undoable |
| `state.get` | none | the project overview (canvases, counts, revision) and `can_undo`/`can_redo` |
| `canvas.inspect` | as MCP's `canvas_inspect` | bounded pages of nodes and edges |
| `screenshot` | none | `{mime: "image/png", width, height, data}` (base64), taken a frame later |

Errors come back as `{"id", "error": {"kind", "message"}}`. Commands only a person may run
(setting or removing API keys, opening files) are refused, as they are over MCP (`docs/mcp.md`).

## Limits

- At most 4 connections at once.
- Messages are at most 4 MiB.
- 64 requests may wait for the app, and each waits at most 30 s.
- The app answers 16 requests per frame.
- Up to 4 screenshots can be pending, each at most 64 MiB of PNG.
