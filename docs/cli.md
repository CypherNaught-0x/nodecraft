# Headless CLI

`nodecraft-cli` runs the same engine commands, planner, provider adapters and result recording
as the canvas. Project inputs remain unchanged. `--json` returns machine-readable reports;
errors go to stderr and a failed run/batch exits nonzero. See `nodecraft-cli --help` for all commands.

## Run and quote

```sh
nodecraft-cli inspect project.ncraft --json
nodecraft-cli quote project.ncraft --input 'Brief=a red fox' --json
nodecraft-cli run project.ncraft --input 'Brief=a red fox' --out results --yes --json
```

Use an existing output folder. Without `--node`, ordinary projects run stale/never-run nodes on
the selected canvas; `--canvas <id>` chooses among multiple canvases, and repeated `--node <id>`
chooses targets with their stale upstream. `inspect` lists their ids. Media exports use the
engine's `history.export`, choosing a fresh filename on collision. Text results appear in the report.

Published Techniques can run from a project or the desktop's local library:

```sh
nodecraft-cli quote project.ncraft --technique 'Swap@2' --input 'Brief=a cat' --input 'Replacement=dog'
nodecraft-cli run 'technique:Swap@2' --input 'Brief=a cat' --input 'Replacement=dog' --out results --json
```

A selector accepts a Technique name, Technique id or published canvas id, optionally followed
by `@version`. Omit the version to choose the latest non-retired version. Ambiguous names fail;
use an id. A selected Technique is placed on a scratch canvas through `technique.place` and
filled through `app.input_set`/`node.param_set`. It cannot combine with `--canvas` or `--node`.
Library targets read the same settings-directory `library` file as the desktop. Missing libraries,
retired versions and unknown selectors fail before provider setup.

Named values follow the real form and parameter schemas:

- Technique inputs and exposed parameters accept their labels or stable names. Prefix a
  conflicting label with `input:` or `param:` to disambiguate it.
- Ordinary source cards accept a unique title or node id: `--input 'Brief=a=b'`. Other card
  parameters use `--input '<title-or-id>.<parameter>=value'`. Whole source titles take precedence
  over splitting on a dot. Duplicate destinations, including aliases, fail.
- Text and choices are literal strings; embedded `=` and newlines are preserved. Booleans are
  `true`/`false`. Integers and seeds use exact decimal strings; floats must be finite. Compound
  controls accept `json:<typed engine Value JSON>`, using the schema from `commands --full`.
- Media values are regular-file paths, optionally prefixed with `@`; bytes and modality are
  validated before provider setup. A many-valued form input accepts `json:["first", "second"]`.

At most 256 bindings, 1 MiB per value and 8 MiB of assignment text are accepted per invocation
or row. Media inputs use the engine's bounded byte-import path: at most 16 MiB per file,
256 MiB across distinct frozen file bodies and 1,024 distinct paths. Repeated paths share bytes.
These input limits are independent of native streaming project assets and provider results.

## Spending

`quote` submits no generation. Paid or partly priced runs require `--yes`; unknown prices stay
unknown. This is explicit consent, not a spending cap. `--estimate` optionally sends job inputs,
including images, to providers for by-usage prices before run consent (up to 62 seconds for
one quote). It submits no generation. Check the report's `allowed`, `complete` and totals fields.

Stopping the process cannot retract a provider request already accepted; it may still be billed.
There is no automatic replay of an interrupted CLI run or retry of an uncertain submission.

## CSV batches

```csv
Brief,Replacement
"a cat, in a garden",dog
"a cat
on a bench",fox
```

```sh
nodecraft-cli batch project.ncraft --technique Swap --csv rows.csv --quote --json
nodecraft-cli batch project.ncraft --technique Swap --csv rows.csv --out results --yes \
  --max-cost USD=0.20 --max-total USD=5.00 --run-timeout 900 --json
```

Headers name the same bindings as `--input`. CSV cells override a command-line default with the
same name, including an explicitly empty cell. Media paths, including command-line defaults in
a batch, resolve relative to the CSV's folder. Text remains literal. UTF-8, an optional BOM,
commas, quoted newlines, doubled quotes, LF and CRLF are supported. Blank physical records are
ignored; a quoted empty record is retained. Headers must be unique and nonempty; row widths
must match. Limits are 8 MiB of CSV, 1,000 data rows, 256 columns, 1 MiB per cell and 128 MiB of
staged typed text across the batch. Repeated command-line defaults are counted before
allocation against a separate 128 MiB expanded-assignment limit; raw assignments are released
after binding validation.

Every row's bindings are validated and media bytes frozen before any provider setup. Then all
rows are quoted and aggregate limits/consent checked before generation. Ordinary batch targets
include cached cards too, so a row matching the saved inputs still returns reused outputs. Each row uses a fresh
document on a shared runner and usage budget, so results and parameters cannot leak between
rows. Quotes are refreshed before each row runs. `--max-cost CUR=amount` limits each row per
currency (repeat for more currencies); `--max-total CUR=amount` caps the whole batch. Limits
refuse unknown prices, excess totals or unlisted currencies rather than treating them as zero.
`--yes` remains required for paid work even when a limit is supplied.

Actual batches require `--out`; `--quote` needs no output folder and creates no row folders.
A unique `batch-<id>` directory contains `row-000001`, `row-000002`, etc.; all row directories
are created before the first generation. A failed row stops subsequent rows unless
`--keep-going` is passed. Timeouts (default 3,600 seconds, range 1–604,800) request cancellation
and allow up to 30 seconds to settle. Reports preserve each row's result/failure/skipped status;
possibly billed or unaccounted results are not reported as successful.

## Providers and ComfyUI

```sh
nodecraft-cli providers list --json
nodecraft-cli providers test openai-images --json
nodecraft-cli comfy servers --json
nodecraft-cli comfy servers studio --test --json
nodecraft-cli comfy import workflow.json --server studio --out imported.ncraft --json
```

Provider lists read readiness from the same settings and OS keychain as the desktop.
`providers test` uses the API keys dialog's free `keys.test` checks, deduplicating shared
credential accounts. Failed/not-ready checks exit nonzero. Adapters without a supported free
probe report `unverifiable`; key presence is never presented as verified. No generation is
submitted. `comfy servers --test` reads each selected server's node list and optional health,
with a 60-second deadline per server; no workflow is submitted. Server configuration is read
without changing settings. Listed URLs omit userinfo, query strings and fragments; invalid
URLs appear as null in JSON and are never echoed. Local and configured Comfy Cloud profiles use the existing adapters.

ComfyUI import accepts API/editor JSON within the engine's 1 MiB command-text envelope
(including metadata, title and exposed-parameter names), validated before server discovery, and uses `comfy.nodes.refresh` followed by
`comfy.workflow.import`. A configured server supplies `/object_info`; missing node classes or
bad workflows fail before publication. `--server` pins the imported card through
`comfy.server.use`. `--project existing.ncraft` adds the card to a copy of that project's selected
canvas; all referenced assets are retained. `--title` names the card; repeated `--param node.input`
selects exposed parameters (otherwise prompts/seeds are selected). The output parent must exist;
an atomic no-clobber publication refuses an existing file even if it appears during the import.

## Native storage

Runs, actual batch rows and imports use native streaming storage with the format layer's quotas
and verified asset leases. Private staging and recovery live beside the output, or beside the
source project when `run` has no output folder (the current folder for a library target).
They do not use the system temp directory. Normal completion/failure discards the private
journal and retires released asset workspaces; a small shared `.nodecraft-assets-v1.lock` remains.
Reader leases can retain backing until they finish. Cleanup failures are reported and retained.
A forced process termination can leave `.nodecraft-cli-*.recovery` and its lock beside that
folder; these are private CLI recovery files, never replacements for the input project.
The native layer's bounded orphan maintenance handles supported abandoned asset workspaces;
it does not silently delete an unknown recovery journal. Pure quotes and inspection retain
the bounded resident decoder (including embedded media) and create no recovery files.
Large v4 archives require the native run/import path; pure quote/inspection retains the
frozen byte-reader limits rather than silently staging a large project.
