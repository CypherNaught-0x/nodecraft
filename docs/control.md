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

The native client gives initial authentication and each replacement connection one five-second
network-admission deadline. TCP connect, every partial hello write/read, and all six replacement
attempts with backoff share that deadline. Hello replies are capped at 4 KiB separately from
screenshot replies. Busy and network failures may retry only admission; once a requested operation
might have been sent, it is never replayed. `Client::set_reconnect_after` retains its existing
behavior, including reconnecting before every call at `Duration::ZERO`.

Reading/re-reading the token file is synchronous filesystem I/O and cannot be interrupted by this
network deadline. Time spent there is counted: if it exhausts admission, no subsequent network
operation starts. Token-file reads stop at 4 KiB plus one detection byte and validate the 64-hex token before
connecting; this is not a bound on filesystem stalls.


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

The current control transport is authenticated TCP. Headless stdio automation is available through
`nodecraft-cli mcp`; a separate stdio control transport is tracked by #269 and is not part of the
live canvas command bridge described below (see the
[control-channel decision](decisions/2026-10-17-control-channel.md)).

### Live canvas commands

`command.run` routes `canvas.fit_all`, `canvas.fit_selection`, `canvas.minimap_toggle` and
`canvas.pan_to` through the app's scoped navigation bridge. A drawn viewport is required; active
editors, popups and gestures keep ownership and refuse navigation. `commands.list` reports fresh
live availability for parameterless commands. These operations change only the view, without
document revisions, undo entries or events. Headless control/MCP sessions remain Unsupported.
Classification is shared with MCP; the live host does not grant access to human-only or paid
commands. Screenshot requests retain their separate asynchronous capture path.
