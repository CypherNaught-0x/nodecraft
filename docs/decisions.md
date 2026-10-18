# Decisions

Decisions others will build on. The product-level ones are in [`plan.md`](plan.md) §6; the ones
taken while building live in [`decisions/`](decisions/), **one file per decision**. File names
start with the date, so the folder lists them oldest first.

Add an entry when you make a choice a later contributor might otherwise undo:

- Create `docs/decisions/<YYYY-MM-DD>-<slug>.md`, where the slug is lowercase ASCII letters,
  digits and hyphens. Include the issue number, e.g. `2026-10-18-generation-history-81.md`.
- Start it with `# <YYYY-MM-DD> · <Title> (#N)`, using the same date as the file name, then the
  decisions as bullets, each opening with the decision in bold.
- To change a decision, add a new entry that says what it replaces. Edit an old one only to fix
  it; don't rewrite history.

Don't add entries to this file: one file per decision keeps two pull requests from conflicting
over the same lines (#295). `cargo xtask ci` checks the names and headings (`cargo xtask
decisions`).
