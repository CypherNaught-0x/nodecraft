<!-- Title: `<summary> (#N)`. Open as a draft right after `/claim` (CONTRIBUTING.md, "Claiming an issue"). -->

Fixes #

## Change

What changed and why.

## Validation

What you ran (`cargo test -p …`, `cargo xtask layers`, …) and the results. For performance or
cost work, give the measured numbers next to the expected ones.

## Evidence (when there is visible behaviour)

Before/after screenshots or a short recording of the canvas, node or editor, with brief steps.
Diagrams don't replace evidence of visible behaviour. Never show API keys or private projects.

## Checklist

- [ ] One topic; rebased on the latest `main`
- [ ] Tests added or updated; `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] No new `unwrap`/`expect`/`panic!` or `unsafe` outside tests
- [ ] New assets or adapted code listed in `ATTRIBUTION.md`
