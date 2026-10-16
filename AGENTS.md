# AGENTS.md: instructions for agents and contributors

NodeCraft is a clean-room, open-source, native **node-based canvas for generative media**: text,
image, video, audio, 3D and document nodes wired into AI models (local ComfyUI or cloud
providers) and deterministic tools, so a creative process can be seen, branched, repeated and
packaged as a reusable Technique. It is **Rust only**: egui/eframe on wgpu for the desktop, the
same Rust compiled to WebAssembly for the browser.

Read this file first, then [`CONTRIBUTING.md`](CONTRIBUTING.md) (the full rules and checklists),
then the `docs/` page for your area. `CLAUDE.md` only points here.

## Product name

**NodeCraft** in everything a person reads: UI, window titles, About, installers, release names,
docs. Machine names are lowercase: crates `nodecraft-*`, binaries `nodecraft` and
`nodecraft-cli`, `.ncraft` project files, `NODECRAFT_*` environment variables. The app id is
`io.github.cyphernaught0x.nodecraft` (Wayland app id, macOS bundle id, Linux desktop/metainfo/
Flatpak ids); never change it, desktops and installers key on it.

## Start every session here

1. `ROADMAP.md`: the current milestone. `docs/plan.md` §7–8: its tasks and exit criteria.
2. The GitHub issue you are working on, and its milestone.
3. `docs/architecture.md` for the crate you will touch; `docs/decisions.md` before undoing
   anything that looks odd (it may be deliberate).
4. `git pull --rebase origin main`, and `cargo xtask ci` to see the tree is green before you
   start. If it isn't, find out why before adding to it.

`plan/` and `log/` are local and gitignored (research notes, dev log). `dev_assets/` holds
reference screenshots and docs of other products: **study, never ship or commit them**.

## Claiming work

Several people and their agents work on NodeCraft at once. A claim keeps two of them from
building the same thing.

1. **Find an issue.** Work is tracked as GitHub issues, labelled `area:*` and grouped in
   milestones (`M0 Skeleton`, `M1 Canvas core`, …). Issues labelled `future` are claimable once
   their dependencies land. Building something bigger than a bug fix with no issue? Open one
   first.
2. **Check nobody has it:** the issue has no assignee and no `in progress` label, and
   `gh pr list --state open --search "#N"` finds no PR for it.
3. **Comment `/claim`** on the issue. The bot (`scripts/claim/claim.js`) assigns you, adds
   `in progress` and replies with your deadline, or tells you who holds it.
4. **Open a draft PR within 72 hours**, titled `<summary> (#N)` with `Fixes #N` in the body. An
   empty commit is fine to start: `git commit --allow-empty -m "claim: #N"`. Without a PR in time
   the claim lapses.
5. **Push at least every 14 days**; a claim whose PR has had no pushes for 14 days is released by
   the daily sweep. Prefer pushing work in progress daily.
6. A draft PR is a claim on its own. If two PRs race for one issue, **the lowest-numbered open PR
   wins**; close yours if it lost, or offer to help on the winner.
7. **`/unclaim`** releases an issue early (claimant or maintainer only).

Never claim by editing files, and never take over someone else's claim without commenting on it
first.

## Never crash

People trust NodeCraft with their work and their API budget; a crash loses both. **This outranks
feature work.** A malformed project file, a bad command or MCP parameter, a hostile provider
response, a corrupt settings file or a full disk must produce an error the user or agent can act
on, never a panic.

- **No panics in non-test code:** no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`,
  `unimplemented!`, and no unchecked indexing. Every crate root starts with
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`;
  `clippy.toml` allows these in tests only. New crates start with the header.
- **No `unsafe`, anywhere.** The workspace sets `unsafe_code = "forbid"`. There are no
  exceptions; if a platform API has no safe wrapper, use a small audited crate that provides one
  (pinned exactly, with a comment saying why), as the Craft apps do.
- **Errors are `Result`s** through the crate's error type and `?`. An unfinished feature returns
  an "unsupported" error.
- **Input-derived numbers are hostile** (files, command params, provider responses, the control
  channel, and arithmetic on any of them): `get()` instead of `[i]`, slice strings only at char
  boundaries, `checked_*`/`saturating_*`, guard division and NaN/∞ casts, cap allocations and
  downloads sized by input, bound recursion (graphs can be cyclic: use seen-sets).
- **Don't cascade:** handle lock poisoning and treat thread joins as `Result`s.
- **Last-resort guard:** command dispatch runs under a panic guard that keeps the document and
  reports the error. It is a safety net, not a licence to panic.
- **Prove it:** every crash fix lands with a regression test that panicked before the fix; the
  `panic_hunt` test fuzzes every command with adversarial params and must stay green.

## Non-negotiables

- **Rust only.** No JavaScript or TypeScript in the product, and no Tauri, Electron or webview
  shells. The one exception is repository tooling that runs on GitHub (`scripts/claim`).
- **Clean-room.** We study other products (node canvases, compositors, model playgrounds) for
  *behaviour and look only*. Never copy their code, shaders, icons, prompts, help text, templates
  or other assets, and never commit their screenshots. Every asset and every file adapted from
  another open-source project gets a row in `ATTRIBUTION.md` (path, what, author, source,
  licence) **in the same change**. Only permissive licences (MIT, Apache-2.0, BSD, Zlib, CC0,
  OFL for fonts); never read or vendor GPL/LGPL/AGPL code.
- **Secrets stay secret.** API keys live in the OS keychain, never in project files, logs, test
  fixtures, screenshots, error messages or commits. Provider requests carry a generic
  `NodeCraft/<version>` User-Agent and nothing personal. Check `git diff` for keys before every
  commit.
- **Everything is a command.** User-visible behaviour is an engine command with a stable dotted
  id (`node.add`, `edge.connect`, `run.selected`), typed JSON params, `enabled` and `run`. The
  UI, shortcuts, palette, CLI, control channel, MCP server and in-canvas agent all dispatch the
  same commands. Programmatic calls never open dialogs.
- **One mutation path.** Commands change the document only through `graph` changesets, which are
  invertible (undo) and serialisable (autosave, audit trail, collaboration). The shell holds only
  view state (pan, zoom, open panels, hover).
- **Data describes the UI.** Node kinds declare ports and a parameter schema; card bodies, the
  inspector, bulk panel, app-mode forms and MCP schemas are generated from it.
- **Runs cost money.** Anything that calls a paid provider shows its cost first, respects
  budgets, and uses idempotency keys so a retry never charges twice. Unit tests never call a real
  provider: use the mock provider in `crates/testkit`; live tests are opt-in
  (`NODECRAFT_LIVE_<PROVIDER>=1`) and never run in CI.
- **Layering** is enforced by `cargo xtask layers` (see the map below). Only `ui-egui` and the
  apps may use egui, eframe, winit, rfd, `craft-ui` or `craft-quit`. Register every new crate in
  `xtask/src/layers.rs`.
- **Never break wasm.** Every library crate builds for `wasm32-unknown-unknown`
  (`cargo xtask wasm`). Filesystem, threads and processes go through platform services.
- **Theme tokens, not literals.** Colours, radii and spacing come from
  `crates/ui-egui/src/theme.rs` (documented in `docs/ui-design.md`).
- **Shared code lives upstream.** Reusable pieces come from
  [storytold/craft-libs](https://github.com/storytold/craft-libs), pinned by an immutable `rev`.
  Fix them there and bump the pin in its own PR rather than forking here.

## Workspace map

```text
crates/
  types media                 L0  pure data types; standalone media codecs (no workspace deps)
  comfy graph                 L1  ComfyUI client; document model + changesets
  nodes catalog actions       L2  node kinds; model catalog; deterministic actions
  exec providers gpu format   L3  run engine; provider adapters; wgpu; .ncraft files
  engine                      L4  session, commands, undo, events, settings, build info
  agent automation ui-egui    L5  in-canvas agent; CLI/MCP/control surfaces; the egui shell
  testkit                         fixtures and the mock provider (dev-dependency only)
apps/
  nodecraft                   desktop app (eframe/wgpu)
  nodecraft-cli               headless CLI with the integrated MCP server
xtask/                        cargo xtask layers | wasm | ci | version
packaging/                    per-OS release packaging (docs/releasing.md)
```

A crate may depend only on strictly lower layers; `media` depends on no workspace crate;
`testkit` is a dev-dependency only.

## Quality gates

Before every commit:

```sh
cargo xtask ci        # fmt --check, clippy -D warnings, tests, layers, wasm
```

While iterating, `cargo test -p <crate>` and `cargo clippy -p <crate> --all-targets -- -D warnings`
are enough. Also run, when they apply:

```sh
cargo test -p nodecraft-engine --test panic_hunt -- --ignored   # added or changed commands
node --test scripts/claim/*.test.js                             # touched the claim bot
packaging/icons.sh                                              # edited assets/app-icon/nodecraft.svg
```

Every change comes with tests: property tests for changesets and the planner, golden graphs run
against the mock provider, fixture-based contract tests for providers, and `egui_kittest` tests
that drive real pointer and key events through the canvas.

## Showing your change

- **Look at UI changes yourself.** Run the app (`cargo run -p nodecraft`) and take a screenshot,
  or write a kittest. Don't ask a human for evidence you can produce.
- PRs with visible changes include real screenshots or a short recording, before and after,
  with a small public-safe example (no private projects, keys, paths or account details).
  Diagrams don't replace evidence of visible behaviour. Attach media to the PR; don't commit it.
- Say in the PR what you changed, how you tested it, and what you measured. If something could
  not be verified (e.g. a platform you couldn't run), say so.

## Landing changes

- **One topic per commit and per PR.** Commits are small, with a clear subject line; a commit
  that fixes an issue says `Fixes #N`.
- Changes land through **pull requests** and GitHub CI (Linux, Windows, macOS). The owner may push
  infrastructure and trivial fixes straight to `main`.
- Stay current: `git pull --rebase origin main` before pushing, resolve conflicts without
  discarding others' work, and re-run the gates after a rebase. **Never force-push `main`** or
  over someone else's commits.
- Never commit `plan/`, `log/`, `dev_assets/`, `target*/`, `dist/`, secrets or generated
  screenshots; check `git status` and `git diff --cached` before committing.
- **Releases are deliberate.** Pushes to `main` are not releases. Follow `docs/releasing.md`
  only when asked: set the version with `cargo xtask version set`, pass the gates, advance the
  `release` branch without force-pushing, and verify the draft release and its artifacts before
  anyone publishes it. Never overwrite a published version.

## Parallel agents on one machine

- Use your own target dir (`CARGO_TARGET_DIR=target/agent-<name>`) so builds don't wait on
  Cargo's lock, and delete it when you are done (each is several GB).
- Edit only the files your task owns. Shared registries (the command list in `crates/engine`,
  the node registry in `crates/nodes`, `xtask/src/layers.rs`) get small, surgical edits; re-read
  them right before editing. Put new commands, node kinds and providers in **new modules**.
- Keep every `Cargo.toml` valid at all times (the `crates/*` glob means one broken manifest breaks
  every build): write manifests outside `crates/`, then `mv` them into place.
- If someone else's in-progress edit breaks the build, wait and retry; don't fix their files.

## Where things are

| What | Where |
|---|---|
| Product scope, architecture, roadmap | `docs/plan.md` |
| Crate map and rules | `docs/architecture.md` |
| Decisions taken and why | `docs/decisions.md` (add one when you make a choice others will build on) |
| Build, test, run | `docs/development.md` |
| Theme tokens, canvas interaction | `docs/ui-design.md` |
| Cutting a release, signing, packaging | `docs/releasing.md` |
| Status | `ROADMAP.md` (update it when a milestone task lands) |
| Checklists: commands, node kinds, providers | `CONTRIBUTING.md` |
