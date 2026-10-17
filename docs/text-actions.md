# Text actions (CPU library)

`nodecraft_actions::text` exposes pure fallible functions and serde request/result types. Stable
action IDs are `text.split`, `text.replace`, `text.concat`, `text.template`. These are library IDs
for future adapters, not registered engine commands. Live nodes and recomputation remain #88.
Requests are owned parameter data; adapters must bound serialized input before deserialization.
The borrowed functions check all input strings before copying, compiling or producing output.

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
output bytes, 16,384 parts/regex candidates/concat inputs, 1,024 template fields and insertions.
Literal replacement has no match-count ceiling: bounded input/output and fixed literal matching
bound its work. Regex candidates are the boundary-qualified matches.
`Limits` can only lower byte and item ceilings, including to zero. All appends and part copies
check budgets first; errors return no partial result. Regex pattern bytes are capped at 8,192
(including escaped literal form and whole-word wrappers), nesting at 64, compiled program at 1 MiB and DFA cache at
256 KiB. Rust `regex` uses a finite-automata engine without backtracking; unsupported lookaround
and backreferences are errors.

Regex work uses syntax-derived bounds (HIR from `regex-syntax`, with the same case flags):
- A finite maximum match width B charges twice the input bytes once for all forward progress,
  then `2*(B+8)` before each iterator search for bounded lookahead and UTF-8 assertion context.
- A top-level repetition of a single character class or single Unicode scalar, optionally wrapped
  in captures, uses B=4 for lookahead accounting. It cannot fail a suffix after a short match:
  there is no following expression or alternative. This includes ordinary `\s+` and `a+`, but
  not `a+b|a`, `(ab)+` or `.*suffix`. Greedy consumption and skipped input advance monotonically;
  lazy/empty suppression also advances. All forward bytes are charged once.
- Other unbounded patterns retain twice-the-remaining-suffix charging before each search, including
  the iterator's possible second search to suppress an adjacent empty match. This protects
  patterns such as `a+b|a` or `.*[^A-Z]|[A-Z]` without pretending returned match length measures
  actual search. Total charged search bytes are capped at 64 MiB; conservative rejection remains
  possible for this deliberately restricted fallback class.

Replacement syntax is parsed/resolved once into at most 16,384 borrowed tokens before searching;
unknown capture references resolve to empty once. Expansion charges each token visit against
1,048,576 steps, including unmatched optional captures that emit no bytes. Budget failures return
`WorkLimit`, never partial output or altered greedy/anchor semantics. Pattern size/compiled NFA
ceilings bound the regex engine's per-byte complexity; these are deterministic work bounds, not
wall-clock deadlines. Diagnostics are fixed public-safe strings and never echo inputs.
