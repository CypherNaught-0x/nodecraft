## 2026-10-18 · Rust CI skips prose-only changes

- **What:** a pull request whose every changed file is documentation, or repository tooling that runs
  outside the build, no longer builds the Rust legs on Linux, Windows and macOS.
  `.github/workflows/ci.yml` gains a `rust scope` job that classifies the change set and outputs
  `rust=true|false`; the three named legs consume it. `claim bot tests` stays unconditional. The
  classifier is `scripts/ci/rust-scope.sh`, tested by `scripts/ci/rust-scope.test.sh`.
- **Why not `on.pull_request.paths-ignore`:** GitHub leaves the checks of a workflow skipped by path
  filtering permanently "Pending" — their words are "Waiting for status to be reported" — and
  `rust (ubuntu-24.04)`, `rust (windows-latest)` and `rust (macos-latest)` are required status checks
  in the repo ruleset with strict mode on. Path filtering would trade a long wait for an infinite
  one. A job skipped by a job-level `if:` reports Success, which clears a required check. That one
  constraint dictates the shape: a job that always runs and decides, and dependents that skip.
  [Troubleshooting required status checks](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).
- **The legs are three named jobs, not a matrix.** The same rule that saves us from `paths-ignore`
  forbids the obvious implementation: a condition that prevents a **matrix** from expanding publishes
  the unexpanded job name, `rust (${{ matrix.os }})`, and none of the three required names ever
  report — the identical dead end. Seen bite two other repositories independently
  ([ifc-lite #3584](https://github.com/LTplus-AG/ifc-lite/pull/3584),
  [agent-glovebox 7e8e1e0](https://github.com/AlexanderMattTurner/agent-glovebox/commit/7e8e1e02f289324708ce2664c7b6195a389067fd)).
  So the legs are three plain jobs with literal names, sharing `.github/actions/rust-leg/action.yml`
  so the platform differences stay in one place. The names are what ruleset strict mode matches, so
  they are load-bearing and cannot be renamed casually.
- **A whitelist, not a blacklist.** The legs are skipped only when every path is on the list;
  anything unrecognised builds. Skipping has to earn its place: a false skip puts an unverified tree
  on `main`, a false run costs a wait.
- **The gate cannot pay for its own exemption**, which took four rules to get right, because a pull
  request can edit the whitelist:
  - CI runs the classifier **as committed on the base revision**, via `git show "$BASE_SHA:..."`, and
    the copy in the pull request's checkout is never the copy that decides. That copy is somebody's
    proposal, including possibly a proposal to skip its own checks. If the base has no such file,
    everything builds.
  - Only a pull request **whose base branch is `main`** may skip. `github.event.pull_request.base.sha`
    is the tip of whatever branch the pull request targets, not necessarily a protected one, so
    against a colleague's feature branch the "base revision" above can be a draft whose whitelist has
    earned nothing. This repository stacks branches, so that is not hypothetical. `BASE_REF` is passed
    in and checked inside the classifier, where the rule it protects lives, and unset is not `main`.
  - CI **runs nothing the pull request wrote before the decision is recorded**, and runs the
    classifier under `env -i` with only the variables it needs. This one arrived late and it was the
    serious finding: an earlier revision ran the pull request's own `rust-scope.test.sh` first, on the
    same job, and any bash in that job could have exported `BASH_ENV` or prepended a fake `git` to
    `$GITHUB_PATH` to make the later classify step print `rust=false` for a pull request full of Rust —
    which, per the rule that makes this design work at all, reports Success on all three required
    checks. The base copy is only trustworthy in an environment the pull request could not arrange.
    Step outputs are attributed to the step id that produced them, so classify-then-nothing-else-first
    is the fix, and `rust-scope.test.sh` asserts that ordering rather than trusting the comment.
  - A change set touching `scripts/ci/`, `.github/workflows/` or `.github/actions/` **forces** Rust,
    even against the classifier's own rules, so editing the rule is never the excuse for not running
    the checks it governs.
  The classifier's tests still run on the pull request's own copy, just *after* the decision is
  recorded: widening the whitelist by mistake fails that job, and a red gate asks for a full build, so
  the failure costs a wait rather than a skip.
- **Fail-open, and how far.** A gate that cannot classify — a missing SHA, an object a shallow
  checkout cannot see, history with no single merge base, a base branch that is not `main` — reports
  `rust=true` and exits 0, because `needs` skips dependents on a *failed* dependency and a skipped
  required check never blocks a merge, so a red gate is worse than an undecided one. The one thing it
  will not do is exit 0 without recording an answer: an unwritable `GITHUB_OUTPUT` fails the job, since
  a silent green gate is exactly the hole this design cannot afford. The consumers therefore ask for
  Rust on anything but a green gate that said `false` — `always() && (needs.scope.result != 'success'
  || needs.scope.outputs.rust != 'false')` — under `always()` for the failed-dependency case, and with
  the result checked as well because a job can record `false` and then go red on a later step.
  `rust-scope.test.sh` asserts both halves of that condition and that no job depends on `scope` without
  `always()`.
- **What is skipped:** `docs/**.md`; `README.md`, `ROADMAP.md`, `CONTRIBUTING.md`, `ATTRIBUTION.md`,
  `AGENTS.md`, `CLAUDE.md`; the licence texts and `NOTICE`; issue and PR templates;
  `assets/*/README.md`; `packaging/release-notes.md`; and `scripts/claim/`, which is Node, has its own
  always-on job and reads no file in this repository. The rest of `scripts/` is **not** skipped: the
  directory holds claim today, but a script added there next month may well be build or release
  tooling, and exempting the whole directory would let that happen quietly.
- **Prose is a listed set of roots, not "any `.md`".** Markdown dropped in a new directory builds
  until someone adds that root, which is the right way round: the alternative is prose whitelisting
  itself by accident. `scripts/ci/rust-scope.test.sh` asserts the roots against an independently
  written copy of the list, so widening them in the script fails a test.
- **Never skipped, with the reason:** `crates/`, `apps/`, `xtask/` are the product; `Cargo.toml`,
  `Cargo.lock`, `clippy.toml`, `rustfmt.toml`, `.cargo/` and `.config/` *define* the gates being
  skipped; `assets/` is compiled in — `include_str!` pulls `assets/catalog/*.toml` into
  `nodecraft-catalog` and `apps/nodecraft/build.rs` watches the app icon; `.gitattributes` pins
  `text eol=lf` on golden JSON the tests parse, so it can break tests while touching no code;
  `packaging/` scripts are run by the release pipeline. Two prose-shaped files are release inputs:
  `packaging/release-notes.md` is passed to `gh release create --notes-file`, and `README.md`, the
  licence texts and `NOTICE` are copied into every artifact by `packaging/env.sh` — those two are
  whitelisted deliberately, because the texts themselves are not compiled and packaging is covered by
  its own changes. `*.md` inside a source tree is never prose: `crates/format/tests/fixtures/v1/README.md`
  and `crates/testkit/tests/golden_driver/README.md` document fixture folders, so they are test
  material.
- **Compared against the merge base**, with `--no-renames` so both ends of a rename are seen. A stale
  branch must not inherit a teammate's code, and a source file moved out of `crates/` must still
  build. Pushes to `main` and manual runs build everything: main is what releases are cut from, a
  manual run is a deliberate full check, and checks reported by a `workflow_dispatch` run do not
  satisfy a required status check at all.
- **Docs-only gets a gate of its own**, because `AGENTS.md` asks a documentation change for `git diff
  --check` and any relevant validator: `docs gate` runs `git diff --check` over the same merge base and
  `cargo run -p xtask -- decisions`. Making the skip safe is not the same as making it unchecked. Note
  that validator is **not** part of `cargo xtask layers` on its own — only `cargo xtask ci` wires the
  two together (`xtask/src/main.rs`) — so the Linux leg names it explicitly and the docs gate runs the
  same command for the case where the legs skip. Between them the two cover every pull request, which
  is what closes the gap where a code change could add a malformed decision record and pass.
- **Consequence, and the merge precondition.** On a prose-only pull request, `claim bot tests` and
  `rust scope` still run, the three `rust (…)` legs report skipped, and the only check that inspects
  the change itself is `docs gate` — which is not a required status check yet. Until it is, a docs-only
  pull request can merge green with nothing *required* having looked at its contents, which is strictly
  weaker than today, where every pull request built the workspace. The ruleset change is therefore part
  of this decision, not a follow-up to it: `rust scope` and `docs gate` join the four contexts now in
  ruleset 25252858, giving `claim bot tests`, `rust scope`, `docs gate`, `rust (ubuntu-24.04)`,
  `rust (windows-latest)`, `rust (macos-latest)`. The owner applies it — tracked as
  [#425](https://github.com/CypherNaught-0x/NodeCraft/issues/425), which carries the exact edit and the
  two throwaway pull requests that prove it.
  - The four existing contexts keep their exact names, so nothing breaks before it lands.
  - Neither new context can leave a pull request Pending: `rust scope` runs unconditionally, and
    `docs gate` is guarded by `always()`, so it is always evaluated and reports Success even when it
    does not run. A required check that a job skips is satisfied, not stuck.
  - That is also why making `docs gate` required does not obstruct code pull requests: on those it is
    skipped, and it becomes load-bearing precisely on the prose-only ones where nothing else looks at
    the change.
  See `docs/development.md`.
