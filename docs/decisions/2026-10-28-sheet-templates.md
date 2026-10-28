## 2026-10-28 · Batch Generate prompt templates expand into rows with `{{name}}` variables (#163)

- **Syntax.** A template is a prompt in which `{{name}}` stands for a named variable's value.
  It is the Template text action's syntax, so one rule holds everywhere in NodeCraft: spaces
  inside the braces are allowed (`{{ colour }}`), `{{{{` is a literal `{{` and `}}}}` a literal
  `}}`. A `}}` outside a placeholder and an unclosed `{{` are refused with the escape to use, so
  a typo never becomes silent text. Card mentions (`@[Title](id)`) and Element mentions
  (`#name`) hold no double braces and pass through untouched. Values are inserted as they are
  and never read as template text again.
- **Names.** 1 to 32 ASCII letters, digits, `_` or `-`. Two variables whose names differ only by
  case are refused, as is a placeholder no variable defines and a text variable the template
  doesn't use (it would only multiply identical rows).
- **Values.** Each variable lists 1 to 100 values explicitly; a value listed twice is refused
  (use copies to repeat rows). An empty text value is a real value that inserts nothing, such as
  "no adjective". Text values hold at most 4096 bytes and a template at most 16 KiB.
- **Image variables** list image source nodes already on the sheet. They are not written in the
  prompt (`{{name}}` for one is refused): each new row gets one of them wired into its first free
  image input, after the shared references it copies, exactly as `sheet.reference_wire` would.
  A node that is already a shared reference can't also be a variable's value.
- **Expansion** (`sheet.expand`) makes one row per combination of the values, their Cartesian
  product, with the first variable varying slowest, and each combination repeated `copies`
  times (1 to 100, adjacent). With no variables, `copies` makes that many rows of the template.
  The count is computed with checked arithmetic; one expansion adds at most 100 rows (the row
  commands' limit) and the sheet still holds at most 1000. Too many is refused with the count,
  its factors ("10 colour × 2 size × 6 copies makes 120 rows") and what to change.
- **Preview before materialising.** `dry_run: true` returns the count, whether it fits, the
  problem if not, and the first five prompts, and edits nothing. A dry run builds the same rows a
  real one would, so "fits" means the expansion works. The table view shows this preview live
  and only enables Expand when it fits.
- **One undo step, no run.** An expansion is one changeset, so one undo removes every row it
  made. It never runs anything: the new rows are quoted and run like any others.
- **Provenance** is recorded on each row under node config `nodecraft.sheet_expansion` (JSON
  text: the expansion's seed, the template, each variable's value or image node, and the copy
  number when there are copies), and `sheet.rows` reports the values and copy. It is left out of
  the recipe hash, so identical rows from one expansion hash alike and editing provenance never
  makes a result stale. `sheet.rows_add` drops it from the row it copies (the new prompt isn't
  the template's); `sheet.rows_duplicate` keeps it (the copy is the same row).
