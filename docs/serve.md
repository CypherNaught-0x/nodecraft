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
| `--webhook-allow <url>` | none | Let runs name a webhook under this URL: the same scheme, host and port, and a path under its path. Repeat for more. Without it, requests can't name webhooks. |
| `--webhook-allow-private` | off | Let webhooks reach loopback, private and link-local addresses (a receiver on your own network). |

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
  "max_cost": {"USD": "0.20"},
  "webhook": "https://hooks.example.com/nodecraft/runs"
}
```

- `version` is optional; the latest is used.
- An input is text, a typed engine value (`{"type": "...", "value": ...}`), a file as
  `{"file": base64, "name": ...}`, or an array of these for an input that takes several.
  Only inputs with `fillable: true` can be given.
- A parameter is text, `true`/`false`, or a typed engine value.
- `max_cost` can only lower the server's limit for this run.
- `webhook` is called when the run ends (below). It must fall under a `--webhook-allow` URL and
  carry no credentials or fragment.
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
let go keeps its place and `n`, marked `"expired": true`. A run that named a webhook has a
`webhook` with its `status` (`pending`, `delivered` or `failed`), its `attempts`, and the last
failure's `error`.

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

## Webhooks

A run that names a `webhook` is POSTed to it once it has succeeded or failed:

```json
{"type": "run.completed", "version": 1, "event": "evt_<run id>", "run": { ...as GET /runs/{id} shows it... }}
```

`version` is the payload's version; it changes only when the payload changes incompatibly.
Outputs' `url`s are paths on this server: fetch the files from there with the token.

### Setting the secret

Deliveries are signed with a secret kept in the OS keychain. Give it on stdin, at least 32
characters; the receiver needs the same one:

```sh
printf 'whsec_%s' "$(openssl rand -base64 32)" | nodecraft-cli webhook-secret
nodecraft-cli webhook-secret --delete     # remove it
```

A secret starting with `whsec_` is the Standard Webhooks form: the signing key is the base64
after the prefix. Any other secret is used as its own bytes. The server reads the secret for
every delivery, so setting a new one rotates it without a restart; a receiver can accept both
during the change. `serve --webhook-allow` refuses to start without a secret.

### Verifying a delivery

Each delivery carries three headers:

| Header | Value |
|---|---|
| `webhook-id` | The event id, the same on every attempt. |
| `webhook-timestamp` | When this attempt was signed, in seconds since 1970 (UTC). |
| `webhook-signature` | `v1,` and the base64 HMAC-SHA256 of `{webhook-id}.{webhook-timestamp}.{body}` with the key. |

A receiver should:

1. Compute the HMAC-SHA256 of the id, a `.`, the timestamp, a `.` and the raw body bytes (before
   any JSON parsing), and compare it with the signature in constant time. The header may hold
   several space-separated signatures; one match is enough.
2. Refuse a timestamp more than a few minutes from its own clock, so a captured delivery can't be
   replayed later.
3. Remember the event ids it has handled and answer a repeat with `2xx` without acting again:
   a retry, or a delivery whose answer was lost, arrives with the same id.
4. Answer `2xx` quickly and do slow work afterwards.

These steps match the Standard Webhooks scheme, so its receiver libraries can verify
deliveries too.

### Retries and failures

An attempt succeeds on a `2xx` answer within 10 seconds. Anything else is retried after 5 s,
30 s, 2 min, 10 min and 30 min, then the delivery is given up as `failed`. Redirects aren't
followed. Before each attempt the host is resolved and its address checked: loopback, private,
link-local and other non-public addresses are refused unless the server runs with
`--webhook-allow-private`, and the request goes to the address that was checked. At most 256
deliveries wait at once.

A delivery never changes the run: a run that succeeded stays succeeded whatever its webhook
did. `GET /runs/{id}` is the source of truth, so a client that misses a delivery (or doesn't
trust one) polls the run. Deliveries waiting when the server stops are lost.
