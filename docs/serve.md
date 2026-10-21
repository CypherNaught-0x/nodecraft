# HTTP serve mode

`nodecraft-cli serve <project.ncraft>` offers a project's published techniques as a small local
HTTP API: list them, start a run with your own inputs, poll it, and download its results. The
project file is read once and never written.

```sh
nodecraft-cli serve fox.ncraft --max-cost USD=0.50
# Serving on http://127.0.0.1:7341/ (token in …/control/serve.json). Stop with Ctrl-C.
```

| Option | Default | What it does |
|---|---|---|
| `--listen <addr>` | `127.0.0.1:7341` | Where to listen. Port `0` picks a free one. Listening beyond loopback exposes your provider keys' spending to anyone with the token. |
| `--token-file <path>` | `control/serve.json` in the app's settings folder | Where the bearer token is written (JSON: `port`, `token`, `pid`). Its folder must be private to you. The file stays after the server stops, and the default is shared, so give each server its own file to run several at once. |
| `--max-cost <CUR=amount>` | none | The most one run may cost in that currency, e.g. `USD=1.50`. Repeat for more currencies. A run that would charge a currency without a limit is refused. |
| `--allow-unpriced` | off | Let runs start whose price isn't fully known (a provider that reports no price). |

Without `--max-cost`, only free runs (local actions, ComfyUI) are served, plus runs whose price
isn't known if `--allow-unpriced` is set. The limit is per run: there is no overall cap yet, so
a client holding the token can start as many runs as it likes, each up to the limit. Keep the
token to yourself, and set provider-side spending limits.

## Requests

Every request carries `Authorization: Bearer <token>`, with the token from the token file; a
new token is made each time the server starts. Bodies are JSON, sent with a `Content-Length`
(chunked bodies are refused). Errors are `{"error": {"kind": ..., "message": ...}}`.

### `GET /techniques`

Each published technique version: `technique` (its id), `version`, `name`, `description`,
`category`, `tags`, `inputs` (`port`, `name`, `port_type`, `fillable`, `example`), `params`
(`spec`, `default`), `outputs`, whether the form is `ready`, and what `blocked` it if not.

### `POST /techniques/{technique}/runs`

```json
{
  "version": 2,
  "inputs": {"in0": "a red fox", "in1": {"file": "<base64>", "name": "fox.png"}},
  "params": {"find": "red"},
  "max_cost": {"USD": "0.20"}
}
```

- `version` is optional; the latest is used.
- An input is text, a typed engine value (`{"type": "...", "value": ...}`), a file as
  `{"file": base64, "name": ...}`, or an array of these for an input that takes several.
  Only inputs with `fillable: true` can be given.
- A parameter is text, `true`/`false`, or a typed engine value.
- `max_cost` can only lower the server's limit for this run.
- An `Idempotency-Key` header (1–128 printable characters) makes a retry safe: the same key
  with the same body returns the run already made (`200`); with a different body it is refused
  (`422`, `idempotency_key_reuse`). A key is remembered as long as its run is (below).

The answer is `202 Accepted` with the run and a `Location: /runs/{id}` header. A full queue
answers `429` with `Retry-After`.

### `GET /runs/{id}`

`status` is `queued`, `running`, `succeeded` or `failed`. Once quoted, `cost` holds the quote's
`totals` and whether it is `complete`; `steps` are the run's steps as `nodecraft-cli run --json`
reports them; a failed run has an `error`. Each output has `n`, `name`, `port`, an `item` index
for list outputs, and either `text`, a `value`, or `mime`, `bytes` and a `url`. A file that was
let go keeps its place and `n`, marked `"expired": true`.

### `GET /runs/{id}/outputs/{n}`

The output's file (`application/octet-stream`, with its real type in `X-Content-Mime`), or its
text. `409` until the run has succeeded; `410` once its files were let go (below).

## Limits and behaviour

- Runs execute one at a time, in order, each in a fresh in-memory copy of the project, so they
  can't affect each other. A run is quoted and checked against the cost limit before anything
  is charged.
- The server remembers the last 256 runs and keeps at most 1 GiB of result files; older runs'
  files are let go first (`"expired": true`). Nothing survives a restart; save what you need.
- At most 8 connections are served at once and 32 runs may wait. A request's head is 16 KiB at
  most and must arrive within 10 seconds; its body is 64 MiB at most, and the whole request
  must arrive within 60 seconds. The token is checked before any of the body is read.
- A run that fails on an internal error is reported as failed; the server keeps serving.
- Stopping the server doesn't stop jobs a provider already accepted; they may still be charged.

Signed completion webhooks are planned; until then, poll `GET /runs/{id}`.
