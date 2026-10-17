# Control channel

`nodecraft --control` lets another process on the same machine drive the running app: run
commands, read the project, and take screenshots of the window. The MCP bridge and test
harnesses use it.

## Connecting

The app listens on `127.0.0.1` on a random port, and writes `control/control.json` into its
settings folder, holding `{"port", "token", "pid"}`. Both the `control` folder and the file are
owner-only. It removes the file when
it quits normally. With in-memory settings (`NODECRAFT_CONFIG_DIR=`), the same JSON is printed to
stdout instead.

A token file can outlive an app that was killed: check that `pid` is still running before
connecting. If another instance with `--control` replaces the file, each instance removes only its
own. The token file is refused in a folder other users can reach. If the channel can't start,
the app still opens and logs why. On Windows the file's access isn't checked; it relies on the
settings folder being in the user's profile. Printing to stdout (in-memory
settings) is for development: release builds on Windows have no console, and on Linux a
launcher's stdout may be logged.

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

- A connection must send `hello` (at most 4 KiB) within 5 seconds in total, and it closes on
  anything else before then.
  Once authenticated, it closes after 10 minutes of silence. A reply that can't be written within
  30 seconds closes it.
- A request the client stopped waiting for (it was told `timeout`) is dropped, never run late.
- A screenshot fails after 10 seconds if the window isn't drawing (for example when minimised).

- At most 4 connections at once.
- Messages are at most 4 MiB.
- 64 requests may wait for the app, and each waits at most 30 s.
- The app answers 16 requests per frame.
- Up to 4 screenshots can be pending, each at most 64 MiB of PNG.

The issue also asks for a stdio transport. The desktop app has no stdin worth reading, and
headless stdio automation is `nodecraft-cli mcp`. A stdio control mode is a follow-up (see
`docs/decisions.md`).

### Live canvas commands

`command.run` routes `canvas.fit_all`, `canvas.fit_selection`, `canvas.minimap_toggle` and
`canvas.pan_to` through the app's scoped navigation bridge. A drawn viewport is required; active
editors, popups and gestures keep ownership and refuse navigation. `commands.list` reports fresh
live availability for parameterless commands. These operations change only the view, without
document revisions, undo entries or events. Headless control/MCP sessions remain Unsupported.
Classification is shared with MCP; the live host does not grant access to human-only or paid
commands. Screenshot requests retain their separate asynchronous capture path.
