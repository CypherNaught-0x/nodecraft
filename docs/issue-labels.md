# Issue labels

Every issue carries an `area:*` label, a milestone where one applies, and **exactly one**
`difficulty:*` label. This page is the rubric for that last one: triage a new issue with it, and
re-rate an existing one when the shape of the work changes.

New issue templates start with `difficulty:medium`; maintainers should re-rate them using this
rubric. Before assigning a new or lapsed claim, the bot requires exactly one recognized difficulty
label (`easy`, `medium` or `hard`) and otherwise asks for triage without changing ownership or
labels. Existing claimants can still acknowledge their claim or `/unclaim` while metadata is fixed.

## The three levels

| Label | What it means |
|---|---|
| `difficulty:easy` | One module, and no design decisions left to make: a known-cause bug, wording or theme tokens, a missing catalog field, wiring one registration line. Tests are a unit test or two in the crate's existing style. |
| `difficulty:medium` | Real design work inside seams the codebase already has, usually two to four crates: a new command, node kind, provider adapter, panel or editor surface that follows an existing pattern. Tests in more than one crate; wasm and the no-panic rules need care but don't change. |
| `difficulty:hard` | A new crate or subsystem, or a change to shared semantics others build on (changesets, the file format, run and queue semantics, the plugin contract). Also anything where a mistake loses money, secrets or work: budgets, idempotent retries and reconciliation, keychains, crash recovery and autosave. Plus performance-sensitive canvas internals, GPU/shader work, wasm-versus-desktop parity, work needing adversarial or `panic_hunt` proof, work that needs a decision record of its own before any code, and work waiting on more than one unbuilt piece. |

## What it measures

Implementation difficulty for someone who has read [`AGENTS.md`](../AGENTS.md) and the docs page
for the area. **Not** priority: where they are set, urgency belongs to the milestone and `future`,
and importance is a maintainer call. Not the size of the ticket text either.

## Tie-breakers, in order

1. **Novelty beats size.** A 20-line change to scheduler fairness or to the archive format is
   `hard`; a large mechanical refactor on a settled pattern is `medium`.
2. **Money, secrets and user data raise the level.** Anything that can double-charge a run, leak a
   key, or silently corrupt or lose a project is `hard`.
3. **"The parts exist, nobody wired them" is `medium`.** Crossing the `ui-egui`/apps/providers seam
   *is* the work. It is `easy` only when one line of registration in a single crate is the whole
   change.
4. **UI is not automatically easy.** A theme-token or wording change is `easy`; a new panel, editor
   or interaction model that must work at every zoom level, with keyboard and screen-reader support
   and `egui_kittest` pointer tests, is `medium` or `hard`.
5. **Unspecified is harder.** If the body still contains an open question or an "or", the
   contributor has to make a judgement call: `medium` at minimum, and `hard` if the answer changes
   the shape of the subsystem.
6. **`future`, or a prerequisite chain, does not by itself make it `hard`.** Judge the work as
   described once its dependencies land.
7. **`good first issue` implies `easy`** — never rate such an issue higher. The converse doesn't
   hold: `difficulty:easy` doesn't promise the onboarding value that `good first issue` does.
8. **Torn between two levels, pick the higher one.** Underestimating burns a claim window;
   overestimating costs a little pride.

The reasoning behind rating difficulty at all, separately from priority:
[`decisions/2026-10-18-issue-difficulty-labels.md`](decisions/2026-10-18-issue-difficulty-labels.md).
