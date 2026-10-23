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

### Live canvas commands

`command.run` routes `canvas.fit_all`, `canvas.fit_selection`, `canvas.minimap_toggle` and
`canvas.pan_to` through the app's scoped navigation bridge. A drawn viewport is required; active
editors, popups and gestures keep ownership and refuse navigation. `commands.list` reports fresh
live availability for parameterless commands. These operations change only the view, without
document revisions, undo entries or events. Headless control/MCP sessions remain Unsupported.
The five `canvas.search_*` commands use the app's existing scoped search host: open the bar,
set a query, step next/previous, or close it. Queries retain the 1024 UTF-8 byte bound and the
incremental per-frame scanner; a response may report scanning before the match is framed.
Search requests also refuse an active minimap gesture. Headless sessions still have no live
search capability.
The tag filter (`canvas.tag_filter_*`), the discussions panel (`canvas.comments_toggle`,
`comment.open`) and Format selection (`canvas.auto_format`) reach the window the same way (#360).
Format selection measures the cards named in its optional `nodes` list, or the selection when
there is none. It is the one live command that edits the document, in one undoable step.

Classification is shared with MCP; the live host does not widen it. Human-only and unclassified
commands stay excluded. Agent-visible `run.start` still requires engine quote/consent validation
and a capable run host; this bridge does not bypass either. Screenshot requests retain their
separate asynchronous capture path.

## Headless stdio

`nodecraft-cli control` serves the same methods against a new empty in-memory project on
stdin/stdout. Its parent process is trusted: no token, token file, socket or keychain service is
created. `hello` with empty params is optional and reports the same app/version/protocol; TCP
still requires its token handshake. This is headless control, not a bridge to a running app.

Requests are processed one at a time. Only newline-delimited JSON replies go to stdout;
diagnostics go to stderr. The shared 4 MiB request limit applies. Blank lines are ignored.
Malformed JSON, invalid UTF-8 and oversized lines return `invalid_input` with a null id, discard
that line, and recover at the next line. EOF processes a final unterminated line and exits
successfully; input/output failure exits nonzero with a diagnostic. There is no socket idle or
handshake timeout on trusted pipes; the parent controls pipe lifetime and must read replies.

Screenshots and live-only canvas navigation return `unsupported` because no window exists.
Human-only and paid command restrictions remain shared with MCP/TCP. Graph edits use the same
validation, revisions and undo. `nodecraft-cli mcp` remains the separate MCP stdio protocol;
`nodecraft-cli mcp --bridge [token-file]` forwards MCP calls to the authenticated running app.
These entrypoints coexist; trusted headless control does not replace the MCP bridge or add a
live viewport to its own in-memory session.
