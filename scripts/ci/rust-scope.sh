#!/usr/bin/env bash
# Decide whether a pull request may skip the Rust legs of CI. Why this exists:
# docs/decisions/2026-10-18-rust-ci-skips-prose-only-changes.md; how it is wired: docs/development.md.
#
# A whitelist, default-deny. The legs are skipped only when EVERY changed path is documentation or
# repository tooling that the build never reads. Unusual, unknown, unreadable: Rust runs. Skipping
# has to earn it, because a wrong skip puts an unverified tree on main and a wrong run only costs
# someone a wait.
#
# Three rules keep the gate from authorising its own bypass. The first belongs to the workflow: it
# runs THIS file as committed on the base revision, before it runs anything the pull request wrote,
# in an environment the pull request could not have arranged. The second: a change set that touches
# this script, its tests, the workflow or a composite action forces Rust even if the rules below
# would skip, so editing the rule is never the excuse for not running the checks it governs. The
# third belongs to the jobs this feeds: they build unless a healthy, green gate said exactly `false`.
#
# A fourth lives in BASE_REF below, and it is checked here rather than only by the caller because the
# thing it protects is the trustworthiness of this file's own contents.
#
# What no design can promise: the workflow YAML is itself part of the pull request, so a pull
# request can rewrite any check in it. That boundary is held by review, which is part of why
# .github/CODEOWNERS claims these paths. What these rules buy is that a skip cannot be arranged by
# accident, or by editing a whitelist nobody reads.
#
# The script exits 0 whenever it reached a decision, including the "I cannot tell" decision, which is
# `rust=true`. It exits non-zero only when it could not record an answer at all, and its consumers
# use `always()`, so a red gate also builds: `needs` skips dependents on a failed dependency, and a
# skipped required check never blocks a merge. A gate that fails silently is the one outcome this
# design cannot afford, so an unwritable output is a hard error.
#
# Why a job and not `on.pull_request.paths-ignore`? GitHub leaves the checks of a workflow skipped by
# path filtering "Pending" forever, and `rust (<os>)` are required status checks in the repo ruleset,
# so a docs-only pull request would sit on "Waiting for status to be reported". A job skipped by a
# conditional reports Success. That holds for a PLAIN job only: a matrix job skipped before it expands
# publishes its unexpanded name, `rust (${{ matrix.os }})`, and the three required names never report.
# So the legs are three plain jobs with literal names, sharing .github/actions/rust-leg/action.yml.
#
# env  BASE_SHA HEAD_SHA   the two ends of the change set; HEAD_SHA is the tree being tested.
#      BASE_REF             the branch the change set is aimed at. A skip is only on the table for
#                           the protected branch, and an unset BASE_REF is treated as "not main".
# out  rust=true|false and base=<the merge base compared>, appended to $GITHUB_OUTPUT.

set -u

# Documentation, listed by root instead of "any .md". Markdown dropped in a new directory runs Rust
# until someone adds that root here, and that is the right way round: the alternative is prose
# quietly whitelisting itself. It also has to be explicit because some prose-shaped files are not
# prose at all: crates/format/tests/fixtures/v*/README.md and
# crates/testkit/tests/golden_driver/README.md describe fixture folders, so they are test material,
# and `docs/` is the only place prose is prose. In a `case` pattern `*` spans `/`, so these cover any
# depth.
is_prose() {
    case $1 in
    docs/*.md) return 0 ;;
    README.md | ROADMAP.md | CONTRIBUTING.md | AGENTS.md | CLAUDE.md | ATTRIBUTION.md) return 0 ;;
    LICENSE-MIT | LICENSE-APACHE | NOTICE) return 0 ;;
    .github/*.md | .github/ISSUE_TEMPLATE/*.md) return 0 ;;
    assets/*/README.md) return 0 ;;
    # Deliberately here although the text is a release input passed to `gh release create
    # --notes-file`: it is not compiled, and anything that reads it is packaging, which forces Rust.
    packaging/release-notes.md) return 0 ;;
    *) return 1 ;;
    esac
}

# Paths the Rust build, its gates and its tests never read.
is_whitelisted() {
    # Checked first, and it does not merely decline to whitelist: it records why, and the loop forces
    # Rust. A change to the pipeline is proven by running the pipeline, and this is what stops a pull
    # request rewriting the whitelist into one that whitelists the pull request.
    case $1 in
    scripts/ci/* | .github/workflows/* | .github/actions/*)
        forced_path='the pipeline decides; it is not decided'
        return 1
        ;;
    esac
    is_prose "$1" && return 0
    # The claim bot: Node, with its own always-on job, and it reads no file in this repository. The
    # rest of scripts/ is deliberately NOT whitelisted even though only scripts/claim exists today,
    # because a script added here next month may well be build or release tooling. Default-deny means
    # the new thing gets listed on purpose rather than inheriting an exemption by its location.
    case $1 in scripts/claim/*) return 0 ;; esac
    return 1
}

# Everything else runs Rust, and these are the ones most likely to be mistaken for prose:
#   Cargo.toml Cargo.lock clippy.toml rustfmt.toml .cargo/ .config/  they define the gates
#   .gitattributes   it pins `text eol=lf` on the golden JSON the tests parse
#   assets/          assets/catalog/*.toml is include_str!'d into nodecraft-catalog; the app icon is
#                    an apps/nodecraft/build.rs input; packaging/icons.sh reads the SVG
#   packaging/       scripts the release pipeline runs, and README.md, the licence texts and NOTICE
#                    are copied into every artifact by packaging/env.sh

run_rust=false
merge_base=
base_arg=${BASE_SHA:-}
head_arg=${HEAD_SHA:-}
base_ref=${BASE_REF:-}

# Nothing can be trusted, so nothing may be skipped.
fail_open() {
    printf 'rust gate: %s; running Rust.\n' "$1" >&2
    run_rust=true
}

# Checked before anything else, and here rather than only in the caller, because what it protects is
# the trustworthiness of the rules this file contains. `github.event.pull_request.base.sha` is the
# tip of whatever branch a pull request targets, and that is not necessarily a protected one: against
# a colleague's feature branch this revision may be somebody's draft, and its copy of the whitelist
# has earned nothing. Empty means unset, which is not main either. An unset variable expanding to an
# empty string is exactly the mistake this guards, so there is no default.
if [ "$base_ref" != main ]; then
    fail_open "the base branch is '${base_ref:-unset}', not the protected main"
elif [ -z "$base_arg" ] || [ -z "$head_arg" ]; then
    fail_open "BASE_SHA and HEAD_SHA must both be set"
elif ! git cat-file -e "${head_arg}^{commit}" 2>/dev/null; then
    # An object we cannot see is a shallow or rewritten history, not an empty change set.
    fail_open "unknown commit $head_arg"
elif ! git cat-file -e "${base_arg}^{commit}" 2>/dev/null; then
    fail_open "unknown commit $base_arg"
else
    # Compare against the merge base, so code a teammate landed on main meanwhile is not counted as
    # this change's. Two merge bases means criss-crossed history and an answer that depends on
    # which one git happened to pick, so that is not an answer either.
    bases=$(git merge-base --all "$base_arg" "$head_arg" 2>/dev/null) || bases=
    base_count=$(printf '%s\n' "$bases" | sed '/^$/d' | wc -l | tr -d ' ')
    if [ "$base_count" = 1 ]; then
        merge_base=$bases
    else
        fail_open "no single merge base for $base_arg and $head_arg (found $base_count)"
    fi

    if [ "$run_rust" = false ]; then
        # -z keeps names with spaces, newlines or unusual bytes intact, and the list goes to a file
        # rather than through a command substitution, which cannot carry NUL. --no-renames lists both
        # ends of a rename, so moving a crate into `crates/` is not read as the prose path it left.
        list=$(mktemp) || list=
        if [ -z "$list" ] || [ ! -f "$list" ]; then
            fail_open "could not create a temporary file to hold the change list"
        elif ! git -c core.quotePath=false diff --name-only --no-renames -z "$merge_base" "$head_arg" >"$list" 2>/dev/null; then
            fail_open "could not diff $merge_base against $head_arg"
        else
            skipped=0
            considered=0
            while IFS= read -r -d '' path; do
                considered=$((considered + 1))
                forced_path=
                if is_whitelisted "$path"; then
                    skipped=$((skipped + 1))
                    printf '  skip  %s\n' "$path"
                else
                    run_rust=true
                    if [ -n "$forced_path" ]; then
                        printf '  rust  %s  (%s)\n' "$path" "$forced_path"
                    else
                        printf '  rust  %s\n' "$path"
                    fi
                fi
            done <"$list"
            # An empty change set is an identical tree: whatever Rust would check already passed at
            # the base. This is what lets the empty commit that opens a claim skip the legs.
            if [ "$considered" -eq 0 ]; then
                printf 'nothing changed between %s and %s.\n' "$merge_base" "$head_arg"
            fi
            printf 'change set: %d of %d paths whitelisted -> rust=%s\n' "$skipped" "$considered" "$run_rust"
        fi
        [ -n "$list" ] && [ -f "$list" ] && rm -f "$list"
    fi
fi

# Recording the answer is the one step with no safe fallback: an empty output would read to the
# consumer as "the gate never decided", which the always() rule sends to a full build at the cost of
# a red job, and a green-but-silent gate is precisely the failure this design cannot afford.
if [ -z "${GITHUB_OUTPUT:-}" ]; then
    printf 'rust gate: GITHUB_OUTPUT is not set, so no decision could be recorded.\n' >&2
    exit 1
fi
if ! printf 'rust=%s\nbase=%s\n' "$run_rust" "$merge_base" >>"$GITHUB_OUTPUT" 2>/dev/null; then
    printf 'rust gate: cannot append to GITHUB_OUTPUT=%s.\n' "$GITHUB_OUTPUT" >&2
    exit 1
fi
exit 0
