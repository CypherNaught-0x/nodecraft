# Contributing to NodeCraft

Thanks for helping. NodeCraft is a native, node-based canvas for generative media, written in
Rust. This page covers how to claim work, the rules every change follows, and step-by-step
checklists for the most common kinds of change.

> [!NOTE]
> NodeCraft is at milestone M0. The crates from [`docs/plan.md`](docs/plan.md) §4.1 exist but are
> mostly empty; some paths below (such as `crates/engine/src/<area>_cmds.rs`) appear in M1. Start
> with [`AGENTS.md`](AGENTS.md), then pick an open issue from the `M0` or `M1` milestone.

## Claiming an issue

Several people (and their coding agents) work on NodeCraft at once. Claims keep two people from
building the same thing.

1. Work is tracked in GitHub issues, labelled by area (`area:canvas`, `area:graph`, `area:exec`,
   `area:providers`, `area:media`, `area:ui`, `area:agent`, `area:infra`) and by exactly one
   `difficulty:easy` / `difficulty:medium` / `difficulty:hard`. Difficulty says what signing up
   costs, not how urgently a change is wanted — that is the milestone and `future`. If you want to
   build something bigger than a bug fix and it has no issue yet, open one first and give it both
   labels. The levels and how to rate them are in
   [`docs/issue-labels.md`](docs/issue-labels.md); re-rate an issue when the shape of its work
   changes.
2. **Comment `/claim` on the issue.** The bot assigns you, adds the `in progress` label and
   replies with your deadline. If someone else holds the issue, it tells you who and when their
   claim lapses.
3. **Open a draft PR within 24 hours** that references the issue (`Fixes #N` in the body,
   `<summary> (#N)` as the title). An empty commit is fine to start:
   `git commit --allow-empty -m "claim: #N"`. If no PR shows up in time, the claim lapses and
   the issue is free again.
4. **Push at least every 14 days.** A claim whose PR has had no pushes for 14 days is released by
   the daily sweep. Both deadlines apply to everyone, including the owner and maintainers.
5. **`/unclaim`** releases an issue early. Only the claimant or a maintainer can release a
   claim.
6. A draft PR counts as a claim on its own. If an unclaimed issue already has someone's open
   PR, `/claim` points you at that PR instead. If two PRs race for one issue, the lowest-numbered
   open PR wins. Close the other one, or offer to help on the winning PR.
7. Mark the PR ready for review when it is done.

The bot lives in [`scripts/claim/claim.js`](scripts/claim/claim.js) and runs from
[`.github/workflows/claim.yml`](.github/workflows/claim.yml). Its tests run in CI:
`node --test scripts/claim/*.test.js`.

### Parallel agents on one machine

- Use your own target dir (`CARGO_TARGET_DIR=target/agent-<name>`) so builds don't wait on
  Cargo's lock. Edit only the files your task owns.
- Make small, surgical edits to shared registries (the command list in `crates/engine`, the
  node registry in `crates/nodes`, `xtask/src/layers.rs`), and re-read them right before editing.
- Put new commands, node kinds and providers in **new modules** rather than growing a shared
  file.
- Keep every `Cargo.toml` valid at all times: the `crates/*` glob means one broken manifest
  breaks everyone's build. To write a manifest, write it outside `crates/`, then `mv` it into
  place.
- If someone else's in-progress edit breaks the build, wait and retry. Don't fix their files.

## Rules that aren't negotiable

- **Language:** Rust only for the product. No JavaScript or TypeScript, and no Tauri, Electron
  or webview shells: the desktop app is egui/eframe on wgpu, and the web build is the same Rust
  compiled to WebAssembly. The one exception is repository tooling that runs on GitHub
  (`scripts/claim`).
- **Licence:** contributions are MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`,
  `NOTICE`). By sending a change you agree to license it under both.
- **Clean-room:** we study other creative tools (node canvases, compositors, editors) for
  *behaviour and look only*. Never copy their code, shaders, icons, model prompts or other
  assets. Third-party assets must be permissively licensed and keep their licence file next to
  them. Every asset, and any code adapted from another open-source project, gets a row in
  `ATTRIBUTION.md` in the same change.
- **Never crash:** non-test code never panics. No `unwrap()`, `expect()`, `panic!`,
  `unreachable!`, `todo!`, `unimplemented!` or unchecked indexing. Return the crate's error type
  and propagate it with `?`. Treat every number that comes from a file, a parameter, a provider
  response or arithmetic on them as hostile: use `get()`, `checked_*`/`saturating_*`, guard
  division and NaN, cap allocations sized by input, and bound recursion. Every crash fix comes
  with a regression test that panicked before the fix. Clippy enforces this: `clippy.toml`
  allows these constructs in tests only.
- **No `unsafe`:** the workspace sets `unsafe_code = "forbid"`.
- **Everything is a command:** new user-visible behaviour is an engine command, which the UI,
  CLI, control channel, MCP and in-canvas agent all dispatch by id. Only pure view state (zoom,
  open panels) lives in the shell.
- **One mutation path:** commands change the document only through `graph` changesets, which
  are invertible and serialisable. Never mutate the document from the UI.
- **Data describes the UI:** node kinds declare ports and a parameter schema. The node body,
  inspector, bulk panel, app-mode form and MCP schema are generated from it. Write bespoke UI
  only for a custom node body or editor.
- **Layering:** `cargo xtask layers` must pass. A crate may depend only on lower layers. Nothing
  below `ui-egui` uses egui, eframe, winit or rfd (shared egui helpers come from `craft-ui`, used
  at the UI layer only). Register every new crate in `xtask/src/layers.rs`.
- **Never break wasm:** crates below the UI layer must `cargo check --target
  wasm32-unknown-unknown` (`cargo xtask wasm`). Filesystem, threads and processes go through
  platform services.
- **Secrets stay secret:** API keys live in the OS keychain, never in project files, logs, test
  fixtures, screenshots or error messages. Provider requests carry a generic
  `NodeCraft/<version>` User-Agent and no personal details.
- **Shared libraries:** reusable pieces come from
  [storytold/craft-libs](https://github.com/storytold/craft-libs) (`craft-ui`,
  `craft-settings`, `craft-atomic`, `craft-quit`, `craft-pdf`), pinned by an immutable git
  `rev` in the workspace `Cargo.toml`. Fix shared code upstream there, then bump the pin, rather
  than forking it here.

## Decision records

Before changing an established choice, read the dated records in [`docs/decisions/`](docs/decisions/).
When a new choice will guide later contributors, add one new
`docs/decisions/YYYY-MM-DD-short-descriptive-slug.md` file, starting with
`## YYYY-MM-DD · Title (#issue)`. Use a unique topic slug and links relative to the new file.
See [`docs/decisions.md`](docs/decisions.md) for the format and historical records. Do not append
to that index or edit a shared list when adding a decision; each independent decision adds only
its own file.

## Style

- `cargo fmt` (160-column lines) and `cargo clippy --workspace --all-targets -- -D warnings`.
- Match the surrounding code. Comments explain *why*, not *what*.
- UI colours, radii and spacing come from the theme tokens, never hard-coded values.
- Commits are small and focused, with a clear subject line. One feature per PR.

## Tests

Every change comes with tests.

- **Changesets:** apply → invert → apply round-trips, as property tests (`proptest`).
- **Planner and scheduler:** routers, batch cross/zip, technique inlining, staleness, pricing and
  budgets, all tested against the **mock provider** in `crates/testkit` (deterministic fake
  media). Unit tests never call a real provider.
- **Providers:** contract tests against recorded fixtures (with secrets scrubbed). Live tests are
  opt-in behind `NODECRAFT_LIVE_<PROVIDER>=1` and never run in CI.
- **Commands:** behaviour, undo/redo, the disabled state, and bad params. The `panic_hunt` test
  fuzzes every command with adversarial params and must stay green.
- **UI:** render offscreen with `egui_kittest` and look at the PNG. Attach before/after
  screenshots to the PR for anything visible.

Before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <crates you touched>
cargo xtask layers
cargo xtask wasm                                        # if you touched a crate below ui-egui
cargo test -p nodecraft-engine --test panic_hunt -- --ignored   # if you added or changed commands
node --test scripts/claim/*.test.js                     # if you touched the claim bot
```

## Checklists

### Adding a command

1. **Id:** a stable, dotted id in the style of its area (`node.add`, `edge.connect`,
   `run.selected`, `technique.publish`). Ids are API: agents and saved macros depend on them.
2. **Logic** goes in the lowest crate that fits (`graph` for document rules, `exec` for running,
   `actions` for deterministic media operations), with unit tests.
3. **Command** goes in an engine module (`crates/engine/src/<area>_cmds.rs`) exposing `specs()`,
   registered in the command list. Fill in `id`, `label`, the default `shortcut`, a params doc
   (what agents read through `commands`), an `enabled` predicate, and `run`. `run` returns a
   JSON result and records exactly **one** undo step (coalesce drags and typing).
4. **Tests** in the module: behaviour, undo/redo, disabled state, bad params. For a
   session-local view command, expose a scoped live host (as `canvas.minimap_toggle` and
   `canvas.prompt_overlays_toggle` do), document headless availability for control/MCP and
   verify that no document revision or undo entry is created.

### Adding a node kind

1. **Descriptor** in `crates/nodes/src/<kind>.rs`: id, category, ports (`PortType` with
   modality and cardinality), `ParamSchema` with defaults and `visible_if` conditions, and
   `mode()` for the input combinations it accepts (e.g. text→image vs image→image).
2. **Planning:** `plan()` turns the node into job specs. It must return `Err` for any invalid
   input combination, never panic.
3. **Register** it in the node registry, and add its "Turn into" entry and quick-add shortcut if
   it has them.
4. **UI:** start with the generated body. Add a custom body in `ui-egui` only when the
   generated one can't express it.
5. **Tests:** planning for each mode, serde round-trip, a golden graph that uses it, and a
   snapshot of its card.

### Adding a model or provider

1. **A new model on an existing provider is data.** Add an entry to the model catalog: id,
   modes and input roles, parameter schema, resolutions, aspect ratios, durations, pricing rule,
   latency hint, quality tier and a one-line description, in `assets/catalog/<provider>.toml`
   (format: `docs/catalog.md`; say in the PR where the prices came from). Run
   `cargo xtask catalog-check`.
2. **A new provider** is a module in `crates/providers/src/<provider>/` behind its own Cargo
   feature. It implements `Provider` (`submit`, `poll`, `cancel`), maps errors onto the shared
   error kinds (blocked content, unsupported input, busy, timeout, budget, auth, network), uses
   idempotency keys where the API supports them, caps download sizes, and sends API keys only
   through `HttpRequest::secret_header` so they never follow a redirect to another host.
3. **Tests:** fixture-based contract tests for submit, progress, success, every error kind and
   cancellation. Add one opt-in live smoke test.
4. **Docs:** add a row to `docs/providers.md` covering auth setup, supported modes and known
   limits.

## Sending a change

1. Claim the issue (above), branch from the latest `main`, and keep the PR to one topic.
2. Run the checks under *Tests*. CI runs them on Linux, Windows and macOS. The exception is a pull
   request aimed at `main` that touches nothing but documentation and repository tooling: it skips
   those three legs and runs the docs gate instead (`docs/development.md`).
3. In the PR, say what you changed, how you tested it and what you measured. For anything
   visible, attach real screenshots or a short recording. Diagrams don't replace evidence of
   visible behaviour. Coding agents should capture that evidence themselves rather than asking a
   human for it.
4. Keep the PR up to date with `main` by rebasing before you push. Never force-push over someone
   else's commits.

## Licence

NodeCraft is dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Unless you explicitly state otherwise, any contribution you intentionally submit for
inclusion in NodeCraft, as defined in the Apache-2.0 licence, is dual-licensed as above, without
any additional terms or conditions.
