# Text actions (CPU library)

`nodecraft_actions::text` exposes pure fallible functions and serde request/result types. Stable
action IDs are `text.split`, `text.replace`, `text.concat`, `text.template`. The app registers an
action node kind under each ID (`nodes::action`, #410), runs it through `engine::action_exec` and
recomputes it live after an edit (#88).
Requests are owned parameter data; adapters must bound serialized input before deserialization.
The borrowed functions check all input strings before copying, compiling or producing output.

## Normative action schema inventory (v1, #162)

This section specifies the action-owned inventory that the shared Rust descriptor contract in
#162 must express. It is not a second descriptor implementation or a promise that generated
UI/CLI/MCP forms already exist. The implementation here remains `TextRequest`, typed options,
`TextResult` and pure functions; the node kinds and their live recompute live in `nodes` and
`engine` (#410, #88). Custom text actions are the bounded expression language of #165
([text-expressions.md](text-expressions.md)), run from text-action plugin packages (#173). Wire fixtures are `crates/actions/tests/fixtures/text_requests.json`.

Stable action and parameter IDs are namespaced and semantic: `text.split.max_parts`, for
example, always means an output-cardinality ceiling, never a grouping size. The wire envelope
uses `action` with one of the four action IDs. Unknown request/option/mode fields and unknown
enum tags are rejected, not preserved or interpreted as defaults. No nullable parameter,
implicit auto, seed, random mode or missing-variable substitution is defined. JSON integer
counts deserialize as Rust `usize`: negative, fractional, string and out-of-target-range counts
are invalid. Positive grouping counts can be as large as `usize::MAX`; the portable native/wasm
subset is 1..=4,294,967,295. A grouping count larger than the bounded input simply gives one
nonempty group, and never authorizes a count-sized allocation.

The tables list wire-required fields. Except `max_parts` for legacy split requests, missing
fields do not acquire defaults during serde deserialization. Explicit JSON null is rejected
for all typed fields, with one unit-enum exception: `paragraphs` can have no `value` or an
explicit `value: null`; both serialize canonically without `value`. Empty text, false, zero,
missing and null are distinct. Empty text can be a valid input; zero is invalid for grouping
and `max_parts`. Boolean false is an explicit setting.

### Input roles, precedence and result shape

Requests contain already resolved inputs. Connection-versus-local-value and bulk-edit precedence
belongs to #162/#88 adapters, not these pure functions; an adapter must pass the resulting value
once, with no further implicit default or override inside the library.

| Action | Input role and cardinality | Result role and cardinality |
|---|---|---|
| `text.split` | `text.split.input`: exactly one scalar text (`input`) | `text.split.parts`: exactly one ordered text collection with 0..effective `max_parts` elements |
| `text.replace` | `text.replace.input`: exactly one scalar text (`input`) | `text.replace.output`: exactly one scalar text, including an empty string |
| `text.concat` | `text.concat.inputs`: one ordered text collection (`inputs`), containing 0..host `items` scalar texts | `text.concat.output`: exactly one scalar text |
| `text.template` | `text.template.template`: exactly one scalar template text (`template`) | `text.template.output`: exactly one scalar text |

Split collection elements and concat inputs retain their left-to-right order. A split is one
operation returning a collection, not one job per part. Concat consumes one ordered collection,
not independent jobs or a cross product. Missing/null collection inputs are invalid; an empty
collection is valid. The host item ceiling is 16,384. `TextResult` is tagged `type: "parts"`
with a string array `value`, or `type: "text"` with a string `value`.

### Split parameters

| Stable parameter ID | Wire mapping | Value/control; required policy | Bounds/units and condition |
|---|---|---|---|
| `text.split.mode` | `mode.mode` | Required enum; display labels Separator, Regular expression, Paragraphs, Lines, Characters; wire `separator`, `regex`, `paragraphs`, `lines`, `chars` | Exactly one branch |
| `text.split.separator` | `mode.value` when `separator` | Required text | Nonempty literal; visible/enabled only in separator mode; counts toward aggregate UTF-8 input bytes |
| `text.split.pattern` | `mode.value` when `regex` | Required regex text | Empty is a valid zero-width regex; visible/enabled only in regex mode; 8,192 effective pattern bytes plus compilation/work limits below |
| `text.split.line_count` | `mode.value` when `lines` | Required positive integer, step 1 | Lines per group, not bytes; visible/enabled only in line mode |
| `text.split.char_count` | `mode.value` when `chars` | Required positive integer, step 1 | Unicode scalar values per group; visible/enabled only in character mode |
| `text.split.max_parts` | `max_parts` | Integer, step 1; persisted on new requests; omitted legacy field defaults to 16,384 | 1..=16,384 parts; always visible |

Paragraphs has no value-bearing parameter. The tagged representation contains only the active
branch; inactive separator/pattern/count values are not persisted or executed. An editor may
keep inactive drafts as view state, but must construct only the chosen branch; hidden values
cannot bypass validation. Choosing a branch requires a value of that branch's type.

`max_parts` is validated before input scanning or regex compilation. Host `Limits` are validated
first, and effective item capacity is `min(max_parts, Limits.items)`. Host zero remains valid:
empty line/character/paragraph splits may return zero parts, while a separator split of empty
input needs one empty part and fails with `ItemLimit`. Exceeding capacity returns `ItemLimit`
with no partial collection; this is a ceiling, not `splitn`, and never truncates or merges a tail.
The standalone borrowed `split` function still uses only the explicit host `Limits.items`.

### Find/replace parameters

| Stable parameter ID | Wire mapping | Value/control; required policy | Bounds/units and condition |
|---|---|---|---|
| `text.replace.regex` | `options.pattern.mode` | Required two-way switch; false maps to `literal`, true to `regex` | Enum wire representation, not a second stored boolean |
| `text.replace.find` | `options.pattern.value` | Required text/regex text | Literal must be nonempty; regex may be empty; effective pattern at most 8,192 UTF-8 bytes; regex editor enabled only for regex mode |
| `text.replace.replacement` | `options.replacement` | Required text, empty permitted | Part of aggregate input bytes; regex capture expansion only when regex is enabled |
| `text.replace.case_sensitive` | `options.case_sensitive` | Required boolean | True is case-sensitive; false uses Unicode simple folding |
| `text.replace.whole_word` | `options.whole_word` | Required boolean | Unicode neighbor-boundary behavior below, including punctuation-edged terms |

Both pattern branches use the same persisted find field and differ by tagged interpretation.
Replacement text is never an extra input port; it is an action-owned setting. Switching regex
mode does not silently escape, unescape or rewrite saved text. Every active value is validated
before processing; hiding regex-specific editing does not suppress literal validation.

### Concat parameters

| Stable parameter ID | Wire mapping | Value/control; required policy | Bounds/units and condition |
|---|---|---|---|
| `text.concat.separator` | `options.separator` | Required text, empty permitted | Inserted only between retained inputs |
| `text.concat.prefix` | `options.prefix` | Required text, empty permitted | Emitted once, including for an empty collection |
| `text.concat.suffix` | `options.suffix` | Required text, empty permitted | Emitted once, including for an empty collection |
| `text.concat.trim` | `options.trim` | Required boolean | Unicode trim before empty testing |
| `text.concat.skip_empty` | `options.skip_empty` | Required boolean | Drops empty results after optional trim, preserving the rest's order |

All concat controls are visible and enabled together. Their text contributes to aggregate input
bytes; the joined expansion is checked against output bytes before every append.

### Template parameters

| Stable parameter ID | Wire mapping | Value/control; required policy | Bounds/units and condition |
|---|---|---|---|
| `text.template.variables` | `fields` | Required bounded key-to-text record; empty object permitted | At most 1,024 entries; keys and values count toward the 1 MiB aggregate UTF-8 input cap; no nested/list/null values |
| `text.template.missing_variable` | Fixed policy, no wire field | Read-only constant `error`, no selectable alternatives | Missing referenced key returns `MissingField`; no empty/fallback substitution |

Map keys are exact UTF-8 strings, serialized in sorted key order by `BTreeMap`; map ordering does
not determine output ordering. Placeholders determine insertion order, including repeated keys.
A key used by a placeholder must be nonempty and contain neither brace; whitespace is significant.
Unused map keys (including empty keys) are allowed and still budgeted. Values are copied once
and are never reinterpreted as template syntax. All template controls are visible together.

### Creation defaults, serialized defaults and identity

The creation scaffold for future adapters is explicit: split starts with empty input,
`lines` grouping 1 and `max_parts: 16384`; replace starts with empty input, `literal` find `""`,
replacement `""`, `case_sensitive: true`, `whole_word: false`; concat starts with `inputs: []`
and `ConcatOptions::default()` (all text empty, both switches false); template starts with an
empty template and `{}` variables. The replace scaffold is an unconfigured editor draft only,
not a declared schema default. An adapter must not emit an executable or canonical Replace
request until the required nonempty literal find is supplied; executing the draft would report
`EmptySeparator`. The other scaffolds are creation values for adapters, not omitted-field serde
defaults or implemented node forms. `TextRequest`
has no Rust `Default`. The only serde omission default is split `max_parts`; constructing a
`TextRequest::Split` in Rust now requires that field explicitly.

All resolved input values/order, active mode values, booleans, template fields and `max_parts`
affect the effective execution snapshot/staleness identity. Host limits that can turn success
into an error must also be recorded by the execution adapter (`engine::action_exec`). Split mode/grouping and
`max_parts` affect collection output shape/admission; no parameter creates paid calls or extra
jobs in this library. The versioned descriptor transport, cache integration, bulk editing and
graph mutation/undo are owned by the node kinds and the engine (#162, #88, #91), not by this
library.

## Semantics

- Split separator/regex discards delimiters and retains empty fields (including empty input). An
  empty literal separator is invalid; an empty regex matches Unicode scalar boundaries. Captured
  delimiters are not returned. Paragraphs discard blank lines (Unicode whitespace only), preserve
  interior line endings, and omit the final CR/LF of each paragraph. Empty/blank input gives no
  paragraphs. Line counts preserve every byte, grouping LF-terminated lines (including CRLF); a
  final newline does not create another line. Character counts mean Unicode scalar values, not
  bytes or grapheme clusters; line/character split on empty input gives no parts. Counts must be
  positive. No split silently truncates.
- Replace is global, non-overlapping. Literal matching escapes regex metacharacters and inserts
  replacement verbatim. Regex matching supports `$1`, `${1}`, `$name`, `${name}`, and `$$`; missing
  or unmatched captures expand to empty. Unbraced names greedily consume ASCII letters, digits
  and underscores. A lone dollar is literal; unclosed or empty `${...}` and nested opening braces
  are `InvalidReplacement`, validated once before any search even when no match exists. Literal
  replacement never parses this syntax. There is no recursive replacement. Case-insensitive
  matching uses Unicode simple case folding, not full multi-character folding (`ß` does not match
  `SS`). Whole-word mode uses Unicode half-boundary assertions: both neighboring
  Unicode scalars must be absent or outside regex's Unicode `\w` class, including combining marks
  and join controls. This permits `C#`, `C++`, `#tag` and `e.g.` while rejecting embedded longer
  words. Assertions remain inside the regex, so `foo|foobar` can select `foobar` when the shorter
  alternative would fail the ending boundary. Captures/anchors retain
  original full-haystack offsets. Case sensitivity is explicit in the options. Empty literal patterns are invalid; zero-width regex matches advance
  safely over UTF-8.
- Concat applies Unicode `trim` before `skip_empty`, joins retained inputs, and always emits
  prefix/suffix, even with no retained items. Options count toward input bytes.
- Templates insert exact named keys using `{{name}}`; no implicit whitespace trimming. `{{{{` and
  `}}}}` escape literal double braces. Missing fields, empty keys, nested braces in keys and
  unmatched double braces are errors; single braces are literal. Values are copied once, never
  interpreted as templates. Unused fields are allowed but included in input budgets.

## Limits and errors

Hard ceilings: 1 MiB aggregate UTF-8 input bytes (including options/field keys and values), 4 MiB
output bytes, 16,384 parts/concat inputs, 1,024 template fields and insertions.
Literal replacement has no match-count ceiling: bounded input/output and fixed literal matching
bound its work. Regex replacement has no item limit; output bytes and replacement work remain bounded.
`Limits` can only lower byte and item ceilings, including to zero. Persisted split `max_parts`
is independently restricted to 1..=16,384 and intersects the host item ceiling; out-of-range
values return `InvalidMaxParts` before processing. All appends and part copies
check budgets first; errors return no partial result. Regex pattern bytes are capped at 8,192
(including escaped literal form and whole-word wrappers), nesting at 64, compiled program at 1 MiB and DFA cache at
256 KiB. Rust `regex` uses a finite-automata engine without backtracking; unsupported lookaround
and backreferences are errors.

Regex work uses syntax-derived bounds (HIR from `regex-syntax`, with the same case flags).
Let `N` be the input byte length. Each admitted shortcut precharges forward progress once,
then charges lookahead before every iterator search, including its final unsuccessful search:

- Finite maximum match width `B`: precharge `2*(N+1)`; each search costs `2*(B+8)`.
- A top-level lazy repetition of one character class or Unicode scalar, optionally captured:
  precharge `2*(N+1)`; each search costs `2*(4+8)`. There is no trailing expression or
  alternative that can force scanning a long suffix before returning a short match.
- Flat sequences of fixed character atoms, zero-width HIR assertions, and greedy unbounded
  repetitions of single consuming atoms: after captures/concatenations are flattened and
  literals expanded into Unicode scalars, let `C` count consuming atoms (each repeated atom
  counts once), `Z` count assertions and `A=C+Z`. Precharge `2*(N+1)*(A+1)`; each search
  costs `2*(4*C+8*Z+8)`. With no assertions this is the prior `2*(4*A+8)` formula.
  Every neighboring pair of repetitions requires a mandatory **consuming** fixed character
  class between them disjoint from **both** repetition alphabets. Assertions never serve as
  delimiters, even when their alphabet is empty. The full delimiter class must be disjoint,
  using case-folded HIR alphabets. Ordinary `a+`, `,\s*`, `\s*,\s*`, `v(\d+)`,
  `\n\s*\n`, `\bfo\w*\b`, `(?m)^\w+` and whole-word wrappers are admitted.

For the flat grammar, a greedy run stops at the first character outside its alphabet and
selects the last admissible suffix position. Assertions only test original-haystack positions;
their result does not depend on the current iterator search start. A suffix position rejected
because of its assertion remains rejected on subsequent searches, so a long rejected tail
cannot yield a succession of earlier short successful matches. Mandatory outside delimiters
separate neighboring runs and prevent later repetitions from repeatedly scanning earlier runs.
The atom-count precharge conservatively covers runs, suffix assertions and fixed-prefix retries;
the factor two covers a final failed search and adjacent-empty suppression. Each assertion
checks at most the neighboring two Unicode scalars, hence its eight-byte context allowance;
this also covers ASCII/Unicode word boundaries, half-boundaries, absolute anchors and CRLF or
multiline line anchors. Captures and offsets retain full-haystack iterator semantics.

The existing top-level lazy single-atom shortcut is unchanged. Lazy repetitions embedded in
assertion-bearing sequences remain conservative, as do repetitions of assertions themselves.

Other unbounded patterns precharge nothing and cost `2*(N-previous_end+1)` before each search.
This includes unbounded alternatives, nested or finite
repetitions within unbounded sequences, multi-character repeated groups, and neighboring repeats
without a delimiter outside both alphabets. Examples include `a+b|a`, `.*[^A-Z]|[A-Z]`,
`(ab)+`, `a*a*`, `a+\ba+` and `a*?suffix`. The factor two covers the iterator's possible second search to
suppress an adjacent empty match. Finite-width expressions take the first shortcut regardless
of their internal shape. Total charged search bytes are capped at 64 MiB; conservative rejection
remains possible for fallback patterns, without treating returned match length as actual search.

Replacement syntax is parsed/resolved once into at most 16,384 borrowed tokens before searching;
unknown capture references resolve to empty once. Expansion charges each token visit against
1,048,576 steps, including unmatched optional captures that emit no bytes. Budget failures return
`WorkLimit`, never partial output or altered greedy/anchor semantics. Pattern size/compiled NFA
ceilings bound the regex engine's per-byte complexity; these are deterministic work bounds, not
wall-clock deadlines. Diagnostics are fixed public-safe strings and never echo inputs.
