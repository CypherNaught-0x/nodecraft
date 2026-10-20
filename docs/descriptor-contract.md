# Descriptor contract

The descriptor contract (#162) is the one statement of what a node's ports and parameters mean.
Node cards, the inspector, the bulk panel, App mode, the CLI and MCP all read it, so a setting
means the same thing everywhere and a value that would be refused at run time is refused while
editing too. This page is the specification. The code is `nodecraft_types::contract` (L0, the
parameter part) and `nodecraft_nodes::NodeContract` (L2, ports and modes); neither depends on egui.

Version 1.0 covers the shared foundation and the source nodes. Version 1.1 adds
[option sources](#option-sources), and 1.2 lets a list input fan out (`fans_out`). Model, action
and editor issues fill it in for their own parameters; see
[What is not in 1.2](#what-is-not-in-12).

## Versions

A contract carries `version: {major, minor}`. A minor version may only add optional fields with
a default that keeps the old meaning, so a newer build reads every older contract. A different
major version is refused with an `unsupported` error and never guessed at. Contract types refuse
fields they don't know rather than ignore them, so an older build refuses a newer minor version's
additions instead of silently dropping them. That costs nothing today: contracts are produced by
the build that reads them and are not stored in project files. Node values are stored, under the
project format's own version rules, and a key the contract doesn't know is kept on the node and
reported, never deleted; it blocks running until the node is migrated or the key removed.

## Parameters

A `Contract` wraps the existing `ParamSchema` (controls, bounds, options, literal defaults,
`required`, `visible_if`) and adds:

- **`namespace`** and **owner.** The namespace is the owner's dotted id (`source.text`,
  `fal.flux_dev`). The owner is `node`, `model` or `action`. A parameter's stable id is
  `<namespace>/<name>`; names stay lowercase identifiers.
- **Per-parameter rules**, at most one per parameter, each naming a parameter that exists:
  - `unit`: `pixels`, `seconds`, `frames`, `percent` and so on. Only numeric controls have a
    unit.
  - `default`: one of four policies that never coincide.
    - `unset`: no value, nothing submitted.
    - `literal`: the schema default.
    - `auto`: the provider chooses, nothing submitted.
    - `random_seed`: a fresh seed per run, recorded with the generation; seeds only.

    Without a policy, a parameter is `literal` if the schema has a default and `unset` if not.
    `auto` and `unset` cannot be combined with a schema default. Zero, the empty string and
    `false` are ordinary explicit values.
  - `visible_when`: the parameter is shown only while this condition holds.
  - `enabled_when`: the parameter is shown but read-only unless this condition holds.

    Both conditions may name earlier parameters only, so they can't form cycles. The schema's
    own `visible_if` still applies as well.
  - `hidden`: `keep` (the default) or `reset`. Either way a hidden or disabled value is never
    submitted. `reset` also marks it for editors to clear.
  - `affects`: whether the effective value changes the result's identity (staleness and cache
    keys; on by default), the cost quote, the output's shape, or the number of jobs.
  - `meaning`: an optional tag such as `seed` or `aspect_ratio`, used by migration below.
  - `options` (1.1): the run-time list the value must come from; see
    [Option sources](#option-sources).
- **Checks**: cross-field rules (`id`, optional `when`, `require`, a public-safe `message`).
  Whenever `when` holds, `require` must hold too.

### Conditions

A condition is data:

- `present`
- `equals`
- `in` (1–64 values)
- `compare` (`lt`, `lte`, `gt` or `gte` against a number; sliders and integer controls only,
  not seeds, whose full `u64` range a floating-point comparison can't hold exactly)
- `all`, `any`, `not`

Values in `equals` and `in` must be valid for the parameter they name, so a condition can't wait
for a value that can never occur.

Conditions see only *active* parameters, meaning visible and enabled ones. A hidden controller
therefore also hides everything that depends on it.

Size limits:

- one condition: depth 8 and 64 terms;
- all of a contract's conditions together: 4,096 terms;
- at most 64 checks.

### Resolution

`Contract::resolve(values, ready)` returns, for every schema parameter in order:

- its stable id;
- its effective value, tagged by source: `unset`, `explicit`, `default`, `auto` or
  `random_seed`;
- `visible` and `enabled`;
- `reset`;
- what it affects.

It also returns every problem found, together rather than only the first, as structured issues
with a code, the parameter or check involved, and a message:

- `invalid_value`: wrong type, or out of range. This includes hidden values, so hiding a control
  can't smuggle a bad value past validation.
- `unknown_param`: a stored key the contract doesn't know.
- `required`: an enabled required parameter with no value. Reported only when `ready`.
- `check`: a cross-field check failed.
- `not_an_option`: an enabled value is not on its fetched option list. Reported only when
  `ready`, and refused by an edit only when it sets that parameter.

Hidden and disabled parameters marked `reset` include ones holding an invalid value, so an editor
can clear the value that blocks running.

`submission()` refuses while any issue is open. Otherwise it returns the enabled parameters'
explicit or literal values, and nothing else. `identity()` lists the enabled parameters that
affect identity. A literal default and the same value typed in explicitly are one identity, so
entering the default never makes a node stale. The `auto` and `random_seed` markers are kept, so
switching a seed from fixed to random does change the identity.

### Running

Planning, running and staleness see a node's values through `NodeKind::validate_instance`. Its
default resolves the node's instance contract and keeps the result as `into_resolved(ready)`:

- **What is sent** is exactly `submission()`: a hidden or disabled value stays on the node for the
  editor, but no job receives it.
- **What a result's identity hashes** is the same set less the parameters that declare
  `identity: false` (`ResolvedParams::unhashed`), so editing a disabled value or one outside
  identity never makes a result stale; the latter is still sent. `generation.image` uses
  `into_identity`, which leaves them out of its visible set (it submits through `submission()`
  itself).
- **What blocks.** An invalid or unknown value is refused even while editing. A missing required
  value or a failed check blocks only a run (`ready`), so paste and migration accept a state a
  run would refuse. So does a value off a list the host has fetched (see
  [Option sources](#option-sources)); a value whose list is unknown does not. The edit commands
  (`node.param_set`, bulk edits, model swaps) still refuse any issue. Each blocked node names the
  first issue.
- **Disabled values** take no part in checks, but a stored value is always type- and
  bounds-checked, enabled or not.
- **Random seeds.** A catalog model's seed whose effective setting is `random_seed` is drawn when
  its step's jobs are handed out, after admission has matched each job to the source's
  submission: one seed per job, within the seed's `max`, derived from the run id, the node, the
  job's index within its step and the parameter, so a retried job sends the same seed (and a
  reused run id repeats them). The drawn seed is recorded with the
  job's settings. It is not part of the result's identity (the `random_seed` marker is), so a new
  draw never makes a result stale, and a node that opts into caching reuses a result with
  whatever seed it was made with. A kind outside the catalog generation path
  (`into_resolved`) still refuses a ready random seed as unsupported, so such a node is blocked
  at validation rather than run with an undrawn seed.
- **What is recorded.** Each result keeps, per provider job and in job order, the settings it was
  submitted with: the provider, which configured instance of it, the provider's model id, the
  parameters exactly as sent (a fixed seed among them), the inputs it was given by role (media
  by reference) and, for a fanned result, the item. Never a credential. The catalog version of
  the model is not recorded yet. A result copied from the cache keeps its settings. Settings too large to keep
  alongside a result are shed rather than failing a paid run, as an over-budget input hash is:
  the recorded inputs go first, so a drawn seed survives; only settings that still do not fit
  are dropped whole, seed included.

A kind that overrides `instance_contract` must override `validate_instance` to match, as
`generation.image` does.

Only a contract that fails validation, or input over the size limits, is an `Err`. Validation
rejects:

- an unsupported version or a malformed namespace;
- duplicate rules, or rules naming parameters that don't exist;
- a unit on a non-numeric control;
- inconsistent default policies;
- conditions that look ahead, name unknown parameters or carry impossible values;
- oversize conditions;
- duplicate or unlabelled checks.

## Ports and modes

A `NodeContract` lists the descriptor's ports in the same order.

- **Inputs** have a `role`, a `min` and an optional `max` wire count. The role says what the input
  is for, such as `prompt`, `start_frame` or `style_reference`; two ports can share a modality but
  never a role (registration checks this). `min` above zero means required. A single port has `max` at most 1, and an absent
  `max` (no limit) is allowed on list ports only. A list input marked `fans_out` (1.2) runs
  the node once per item of a batch wired into it, the way a single input does, instead of
  collecting the batch into one list; its other wires join each item in order. Text Generate's
  `prompt` fans out, so a batch of prompts makes one answer per prompt, while media roles still
  collect. Media Generate doesn't fan out yet (#519). Only a list input may be marked;
  `fans_out` is omitted when false.
- **Outputs** have a role and say whether they hold a collection, matching the port.
- **Modes** name the wire counts a mode needs on some inputs; an input a mode doesn't name
  accepts any count its own bounds allow. For example:
  - `first_last`: one wire on each frame port;
  - `first_frame`: a start frame and no end frame.

`resolve_mode(wires, ready)` checks each input's bounds (minimums only when `ready`), then picks
the one declared mode the wires fit. If no mode fits, the inputs are unsupported when `ready`,
and the node simply has no mode yet otherwise. Registration rejects two modes that could both fit
one wiring, so a valid contract is never ambiguous; resolving an unvalidated one names both modes
in its error. A kind with
no declared modes keeps deciding through `NodeKind::mode`.

**Wire order.** Wires into one port are ordered by edge `order`, then by edge id. Jobs receive
their inputs in that order, and prompt and reference selection follows it, so reordering wires
is the one way to change it and storage order never does.

A contract must describe its own descriptor exactly. Registration rejects a kind whose contract
has any of these:

- ports out of order or missing;
- impossible bounds;
- a mismatched collection flag;
- a role used by two inputs;
- an unknown, duplicated, impossible or overlapping mode;
- a schema that differs from the descriptor's.

`NodeKind::contract(descriptor)` defaults to one derived from the descriptor:

- each role is the port's name;
- inputs are optional;
- single ports take at most one wire;
- parameters get no extra rules.

Source kinds declare their single `source` mode. Their value parameter affects the output's
shape, and their output's role is `value`.

## Precedence

- **Values**, lowest to highest:
  1. schema defaults, applied by `resolve`;
  2. Technique defaults;
  3. model presets;
  4. the node's own values.

  `overlay` merges layers 2–4. A higher layer can replace a value but not remove one; clearing
  a Technique default back to unset needs an explicit policy in a later version. A bulk edit writes the node's own values through the usual
  `node.param_set` changesets, as one undo step per logical edit.
- **Connected text and a local prompt** combine through `TextPrecedence`:
  - `connected_first` (the default);
  - `local_first`;
  - `connected_replaces`: connected text replaces the local prompt; if every wire is empty, the
    local prompt is used;
  - `local_only`: wires are ignored entirely.

  `compose_text` sorts wires by order, then tie-break id, skips empty pieces, and refuses two
  pieces at one position. The result doesn't depend on the order its inputs arrive in.

## Model swaps and descriptor upgrades

`migrate(from, to, values)` carries explicit values across. A value carries over only when all of
these hold:

- both contracts have the parameter;
- with the same kind of control and the same unit;
- with matching meaning tags, if either side has one;
- across different namespaces (a model swap), both sides tag it with the same meaning;
- and the value is valid in the target.

Within one namespace (a descriptor upgrade) the owner's own name is its promise. Across owners, a
matching name alone is never taken as proof of a matching meaning.

Nothing is coerced. Every value left behind is reported with its reason: `not_in_source`,
`not_in_target`, `control_changed`, `unit_changed`, `meaning_changed`, `unverified_meaning` or
`invalid_in_target`. The caller shows the report and applies the result
through a changeset.

## Bulk edits

`Contract::shared_params(contracts)` lists the parameters every selected contract has with a
compatible meaning, under the same rule as migration. For each of them, `shared_value` reports
one of:

- `same`, with the value;
- `mixed`: targets disagree, or only some have a value;
- `unset`.

The bulk panel (#71) edits only shared fields, and validates each target's new values with that
target's own contract.

## Option sources

Some allowed values are only known at run time: the checkpoints a ComfyUI server has installed,
the voices an account owns. A rule's `options: {id, scope?}` names such a list. `id` is a dotted
lowercase source id (`comfyui.checkpoints`); `scope` names an earlier single-line text, choice
or model parameter whose value narrows the list (a checkpoint's LoRAs). Only single-line text
and model parameters take a source.

The host fetches the lists and installs them in the session as `OptionLists`, keyed by the
contract's namespace, the source id and the scope value. The store is bounded (16,384 values a
list, 256 lists, 16 MiB of text); a list that breaks the bounds, or has duplicate or empty
values or blank labels, is refused and the previous list kept.

- **Unknown is not empty.** A list the host has not fetched, could not fetch, or whose scope
  parameter has no value refuses nothing; the provider checks the value when the job is
  submitted. Each such parameter's resolution says `options: {source, scope, known: false}`.
- **A value off a known list is a `not_an_option` issue.** An edit refuses it only when it sets
  that parameter, so a checkpoint removed from the server never stops the rest of the node
  being edited, and is fixed by picking another value or clearing it.
- **Disabled parameters draw from no list**, like every other check on a disabled value.

Planning a run consults the lists the host has installed: `not_an_option` blocks the run before
anything is admitted, priced or charged, and before an idempotency key exists. What the host has
not fetched still refuses nothing, so a run is never refused against a list it never refreshed,
and the provider stays the authority when the job is submitted. Planning asks the same resolver the
other paths ask, `resolve_with`: with `ready` set, as `node.describe` does, while the edit,
bulk-edit and `node.options` paths ask it unset, which is why the same value refuses an edit only
when it sets that parameter. A disabled parameter therefore draws from no list and a scoped list
is narrowed by its scope value in exactly one place. Work that names no list is planned and
admitted by the rules it already had, and a blocked run names the code rather than the parameter:
`node.describe` and `node.options` are what say which field to fix.
`node.options {canvas, id, param}` returns a stored node's list for one parameter, as
`{source, scope, known, options: [{value, label}] | null}`.

**Fetching (#529).** `options.refresh {}` asks the host's `options.fetch` service for every
unscoped list the catalog's contracts name; the handler does no I/O and the host answers
`{pending: id}`. The desktop app fetches the ComfyUI server's `/object_info` off the UI thread
(the adapter's own settings and optional token) and installs each list once per model that names
it, in the server's order, without empty, oversized (over 4 KiB) or repeated values. A list over
16,384 values is refused whole rather than truncated. `options.status {}` reports the last fetch:
`{id, installed: [{owner, source, values, skipped}], refused: [{owner, source, reason}], error}`.
A failed fetch and every refused list keep what was installed before. The source ids a ComfyUI
server answers are `comfyui.checkpoints`, `comfyui.loras`, `comfyui.vaes`,
`comfyui.upscale_models`, `comfyui.samplers` and `comfyui.schedulers`; scoped sources are refused
with a reason until a fetch can narrow them. The built-in SDXL row's `checkpoint` names
`comfyui.checkpoints`.

## Surfaces

`node.describe {kind}` returns a kind's contract. `node.describe {canvas, id}` returns a stored
node's instance contract, its resolution and the settings its active result was made with (an
empty list when it has none). Recorded inputs can be large (up to the node's own size limit), so
a caller that only needs the contract should describe the kind. The command is read-only and available to agents, so the CLI and
MCP see the same fields, conditions and issues as the inspector.

## What is not in 1.2

Version 1.0 defines and tests the contract, validates every built-in kind's contract at
registration, exposes it through `node.describe`, and plans and runs every kind through it (see
[Running](#running)). Version 1.1 adds option sources and the store a host fills; 1.2 adds
fan-out on list inputs.

These are tracked on #162 and on the issues that need them:

- fetching provider inventories (voices, account models), lists narrowed by a scope, and
  fetching from the CLI;
- typed nested records and lists;
- generated UI reading rules (enabled state, reset) and the bulk panel itself (#71);
- per-provider parameter inventories.

Each of these is added as a minor version where it is optional, or a major version where it
changes meaning.

## Migration and override admission

Before migration reports or layered overrides copy supplied values, they admit the complete
borrowed input: at most256 distinct parameter names, names of at most128 bytes, aggregate name
bytes and aggregate text/MIME bytes each at most1 MiB, and at most4096 Value items with list depth
at most32. Item, text and name accounting spans all layers, including values later overwritten.
The bounded traversal reserves its pending frontier before extending it. Unknown names retain
their original spelling and need not satisfy known-schema identifier grammar; bounded unknown
migration values still receive their normal dropped-value report. Oversized input returns a
bounded InvalidInput error before any output copies, rather than producing a partial report.
Ordered numeric conditions compare signed and unsigned integers exactly against their finite
numeric threshold, including full-width values beyond floating-point integer precision. Seed
controls retain their existing equality/presence semantics.
