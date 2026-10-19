# Descriptor contract

The descriptor contract (#162) is the one statement of what a node's ports and parameters mean.
Node cards, the inspector, the bulk panel, App mode, the CLI and MCP all read it, so a setting
means the same thing everywhere and a value that would be refused at run time is refused while
editing too. This page is the specification. The code is `nodecraft_types::contract` (L0, the
parameter part) and `nodecraft_nodes::NodeContract` (L2, ports and modes); neither depends on egui.

Version 1.0 covers the shared foundation and the source nodes. Model, action and editor issues
fill it in for their own parameters; see [What is not in 1.0](#what-is-not-in-10).

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

Hidden and disabled parameters marked `reset` include ones holding an invalid value, so an editor
can clear the value that blocks running.

`submission()` refuses while any issue is open. Otherwise it returns the enabled parameters'
explicit or literal values, and nothing else. `identity()` lists the enabled parameters that
affect identity. A literal default and the same value typed in explicitly are one identity, so
entering the default never makes a node stale. The `auto` and `random_seed` markers are kept, so
switching a seed from fixed to random does change the identity.

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
  `max` (no limit) is allowed on list ports only.
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

## Surfaces

`node.describe {kind}` returns a kind's contract. `node.describe {canvas, id}` returns a stored
node's instance contract and its resolution. The command is read-only and available to agents,
so the CLI and MCP see the same fields, conditions and issues as the inspector.

## What is not in 1.0

Version 1.0 defines and tests the contract, validates every built-in kind's contract at
registration, and exposes it through `node.describe`. Planning and running still resolve values
through `ParamSchema::resolve`. Moving the run path onto `Resolution::submission()` comes next,
and must land before any kind declares `visible_when`, `enabled_when` or checks. Until then those
rules have no effect on what is sent.

These are tracked on #162 and on the issues that need them:

- planning and running through the contract's resolution;
- persisting a generation's effective settings snapshot;
- dynamic, model-scoped option sources;
- typed nested records and lists;
- generated UI reading rules (enabled state, reset) and the bulk panel itself (#71);
- per-provider parameter inventories.

Each of these is added as a minor version where it is optional, or a major version where it
changes meaning.
