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
  and underscores. A lone dollar is literal; an unclosed `${` is an error when expanded. There
  is no recursive replacement. Case folding and whole-word boundaries use regex Unicode rules;
  whole word wraps the entire expression in `\b(?:expression)\b`. Case sensitivity defaults are
  explicit in the options. Empty literal patterns are invalid; zero-width regex matches advance
  safely over UTF-8.
- Concat applies Unicode `trim` before `skip_empty`, joins retained inputs, and always emits
  prefix/suffix, even with no retained items. Options count toward input bytes.
- Templates insert exact named keys using `{{name}}`; no implicit whitespace trimming. `{{{{` and
  `}}}}` escape literal double braces. Missing fields, empty keys, nested braces in keys and
  unmatched double braces are errors; single braces are literal. Values are copied once, never
  interpreted as templates. Unused fields are allowed but included in input budgets.

## Limits and errors

Hard ceilings: 1 MiB aggregate UTF-8 input bytes (including options/field keys and values), 4 MiB
output bytes, 16,384 parts/replacement matches/concat inputs, 1,024 template fields and insertions.
`Limits` can only lower byte and item ceilings, including to zero. All appends and part copies
check budgets first; errors return no partial result. Regex pattern bytes are capped at 8,192
(including escaped literal form), nesting at 64, compiled program at 1 MiB and DFA cache at
256 KiB. Rust `regex` uses a finite-automata engine without backtracking; unsupported lookaround
and backreferences are errors. Before each regex-mode iterator search, twice the remaining suffix bytes plus boundary overhead is charged
against a 64 MiB aggregate search-work ceiling (an iterator can search twice when suppressing
adjacent empty matches). Regex replacement parsing has a separate 16 MiB aggregate scanned-byte
ceiling. Conservative work accounting may reject otherwise valid large/high-match inputs; it
returns `WorkLimit`, never partial output or altered greedy/anchor semantics. Fixed literal split/replace do not charge suffix scans; they retain their input/output/item
budgets. These are work
ceilings rather than a wall-clock deadline. Diagnostics are fixed public-safe strings and never echo inputs.
