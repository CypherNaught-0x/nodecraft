#!/usr/bin/env bash
# Tests for scripts/ci/rust-scope.sh: build throwaway repositories in a temporary directory and
# assert the decision for each change set. Run `bash scripts/ci/rust-scope.test.sh`.
#
# These run on the `rust scope` job of .github/workflows/ci.yml, AFTER that job has recorded its
# decision, so a whitelist change that would let a code change through is caught by the change that
# makes it, while the code under test stays unable to influence the answer.
#
# Every repository here is created under a temporary directory and every destructive step checks it
# first with owned(). An earlier version of this suite handed back an empty path from repo() and
# committed into the caller's own checkout, which is the one thing a test for a CI gate may never do.

set -u

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
gate="$here/rust-scope.sh"
work=$(mktemp -d) || {
    printf 'FATAL could not create a temporary directory; refusing to run.\n' >&2
    exit 1
}
# Resolve it the way the filesystem does, so `owned()` below compares real locations rather than a
# string a caller could walk out of with a `..`.
work=$(CDPATH= cd -- "$work" && pwd -P) || exit 1
trap 'rm -rf "$work" 2>/dev/null || true' EXIT

pass=0
fail=0

# put <dir> <path> <content>: write a fixture file, creating its parent directories.
put() {
    mkdir -p "$(dirname "$1/$2")" && printf '%s\n' "$3" >"$1/$2"
}

# repo <name>: a fresh repository, one commit, holding one real example of every path the gate
# distinguishes. Only paths that exist in this repository are used, so a fixture that rots into a
# fiction fails as "no such fixture" rather than silently testing a file nobody has.
repo() {
    local d=$work/$1
    [ -d "$d" ] && return 1
    mkdir -p "$d" || return 1
    put "$d" README.md '# Readme'
    put "$d" ROADMAP.md '# Roadmap'
    put "$d" CONTRIBUTING.md '# Contributing'
    put "$d" AGENTS.md '# Agents'
    put "$d" CLAUDE.md '@AGENTS.md'
    put "$d" ATTRIBUTION.md '# Attribution'
    put "$d" docs/development.md '# Development'
    put "$d" docs/decisions/2026-10-18-example.md '# Decision'
    put "$d" LICENSE-MIT 'MIT'
    put "$d" LICENSE-APACHE 'Apache'
    put "$d" NOTICE 'Notice'
    put "$d" .gitignore 'log/
plan/
*.tmp'
    put "$d" .gitattributes 'crates/testkit/tests/golden/*.json text eol=lf'
    put "$d" clippy.toml '[lints]'
    put "$d" rustfmt.toml 'edition = 2024'
    put "$d" .cargo/config.toml '[alias]'
    put "$d" .config/nextest.toml '[profile.ci]'
    put "$d" .github/pull_request_template.md '# PR'
    put "$d" .github/ISSUE_TEMPLATE/bug_report.md '# Bug'
    put "$d" .github/workflows/ci.yml 'name: CI'
    put "$d" .github/actions/rust-leg/action.yml 'name: leg'
    put "$d" .github/dependabot.yml 'version: 2'
    put "$d" crates/engine/src/lib.rs 'fn x() {}'
    put "$d" crates/format/tests/fixtures/v1/README.md '# v1 fixtures'
    put "$d" crates/testkit/tests/golden/baseline.json '{}'
    put "$d" crates/testkit/tests/golden_driver/README.md '# golden driver'
    put "$d" apps/nodecraft/src/main.rs 'fn main() {}'
    put "$d" apps/nodecraft/build.rs 'fn main() {}'
    put "$d" xtask/src/main.rs 'fn main() {}'
    put "$d" Cargo.toml '[workspace]'
    put "$d" Cargo.lock 'version = 6'
    put "$d" assets/catalog/comfyui.toml '[providers]'
    put "$d" assets/app-icon/nodecraft.svg '<svg/>'
    put "$d" assets/app-icon/README.md '# Icons'
    put "$d" assets/shaders/sky.wgsl 'fn sky() {}'
    put "$d" packaging/release-notes.md '# Notes'
    put "$d" packaging/icons.sh '#!/bin/sh'
    put "$d" scripts/claim/claim.js '// bot'
    put "$d" scripts/claim/claim.test.js '// test'
    put "$d" scripts/make_charts.py '# tooling'
    put "$d" scripts/ci/rust-scope.sh '# gate'
    put "$d" scripts/ci/rust-scope.test.sh '# gate tests'
    put "$d" .github/workflows/release.yml 'name: Release'
    git -C "$d" init -q -b main . &&
        git -C "$d" config user.email test@example.invalid &&
        git -C "$d" config user.name test &&
        git -C "$d" config commit.gpgsign false &&
        git -C "$d" add -A &&
        git -C "$d" commit -qm base &&
        printf '%s\n' "$d"
}

# owned <dir>: refuse to touch anything but a repository this suite created. Resolved before
# comparing, because matching the literal "$work" prefix would let "$work/../elsewhere" through.
owned() {
    local resolved
    [ -n "${1:-}" ] || return 1
    resolved=$(CDPATH= cd -- "$1" 2>/dev/null && pwd -P) || return 1
    case $resolved/ in
    "$work"/*) [ -d "$resolved/.git" ] ;;
    *) return 1 ;;
    esac
}

report() { # <label> <expected> <actual> [<detail>]
    if [ "$3" = "$2" ]; then
        printf 'ok    %-56s %s\n' "$1" "${4:-}"
        pass=$((pass + 1))
    else
        printf 'FAIL  %-56s expected %s, got %s\n' "$1" "$2" "$3"
        [ -n "${5:-}" ] && printf '      %s\n' "$5"
        fail=$((fail + 1))
    fi
}

# decide <dir> <base> <head> [<script>] [<base_ref>]: run the gate as the workflow does. The default
# base_ref is main because that is the only branch a skip is on the table for; the cases that pass
# something else are testing that everything else builds. Sets decision, status, log.
decision=
status=0
log=
decide() {
    decision=
    log=
    if ! owned "$1"; then
        printf 'FAIL  refused to classify a repository this suite did not create\n'
        fail=$((fail + 1))
        return
    fi
    : >"$work/gho"
    # `${5-main}`, not `${5:-main}`: an explicitly empty base ref is one of the cases under test, and
    # `:-` would quietly turn it into main and assert nothing.
    log=$(cd "$1" && GITHUB_OUTPUT=$work/gho BASE_SHA=$2 HEAD_SHA=$3 BASE_REF=${5-main} bash "${4:-$gate}" 2>&1)
    status=$?
    decision=$(sed -n 's/^rust=\(.*\)$/\1/p' "$work/gho" | tail -1)
}

# check <label> <expected> <dir> [<file>...]: append to those files, commit, decide against main.
check() {
    local label=$1 expect=$2 d=$3
    shift 3
    local f
    if ! owned "$d"; then
        printf 'FAIL  %-56s no such fixture repository\n' "$label"
        fail=$((fail + 1))
        return
    fi
    for f in "$@"; do
        if [ ! -f "$d/$f" ]; then
            printf 'FAIL  %-56s no such fixture: %s\n' "$label" "$f"
            fail=$((fail + 1))
            return
        fi
        printf 'changed\n' >>"$d/$f"
    done
    local base
    base=$(git -C "$d" rev-parse HEAD)
    git -C "$d" add -A >/dev/null 2>&1
    if ! git -C "$d" commit -qm change >/dev/null 2>&1; then
        printf 'FAIL  %-56s nothing to commit\n' "$label"
        fail=$((fail + 1))
        return
    fi
    decide "$d" "$base" HEAD
    report "$label" "$expect" "$decision" "rust=$decision" "$log"
}

# remove <label> <expected> <dir> <file>...: deletions are changes too, and the ones that matter go
# the wrong way in a naive rule: deleting a .rs is a change to the build, deleting docs is not.
remove() {
    local label=$1 expect=$2 d=$3
    shift 3
    local f
    if ! owned "$d"; then
        printf 'FAIL  %-56s no such fixture repository\n' "$label"
        fail=$((fail + 1))
        return
    fi
    for f in "$@"; do
        if [ ! -f "$d/$f" ]; then
            printf 'FAIL  %-56s no such fixture: %s\n' "$label" "$f"
            fail=$((fail + 1))
            return
        fi
        git -C "$d" rm -q -f "$f" || return 1
    done
    local base
    base=$(git -C "$d" rev-parse HEAD)
    git -C "$d" add -A >/dev/null 2>&1
    git -C "$d" commit -qm 'delete files' >/dev/null 2>&1 || {
        printf 'FAIL  %-56s nothing to commit\n' "$label"
        fail=$((fail + 1))
        return
    }
    decide "$d" "$base" HEAD
    report "$label" "$expect" "$decision" "rust=$decision" "$log"
}

# --- Skippable: documentation, and tooling with its own always-on job. ---
s=$(repo skip) || exit 1
check "top-level README.md" false "$s" README.md
check "ROADMAP.md" false "$s" ROADMAP.md
check "CONTRIBUTING.md and ATTRIBUTION.md" false "$s" CONTRIBUTING.md ATTRIBUTION.md
check "AGENTS.md and CLAUDE.md" false "$s" AGENTS.md CLAUDE.md
check "a docs/ page" false "$s" docs/development.md
check "a docs/decisions/ record" false "$s" docs/decisions/2026-10-18-example.md
check "licence texts and NOTICE" false "$s" LICENSE-MIT LICENSE-APACHE NOTICE
check "issue and PR templates" false "$s" .github/ISSUE_TEMPLATE/bug_report.md .github/pull_request_template.md
check "assets/app-icon/README.md" false "$s" assets/app-icon/README.md
check "packaging/release-notes.md" false "$s" packaging/release-notes.md
check "claim bot (has its own job)" false "$s" scripts/claim/claim.js
check "claim bot tests" false "$s" scripts/claim/claim.test.js
check "all prose at once" false "$s" README.md ROADMAP.md docs/development.md NOTICE
check "prose plus scripts" false "$s" README.md scripts/claim/claim.js

# --- Only what is listed in scripts/ is skippable; the rest of it builds. ---
# This is the rule that has to stay narrow. scripts/claim is Node and has its own always-on job; a
# script added under scripts/ next month may well be build or release tooling, and the whole directory
# being exempt would let that happen quietly.
check "a new tool elsewhere in scripts/ builds" true "$s" scripts/make_charts.py

# --- Never skippable: one of these in the set builds all three legs. ---
r=$(repo run) || exit 1
check "one .rs under crates/" true "$r" crates/engine/src/lib.rs
check "xtask source" true "$r" xtask/src/main.rs
check "a build.rs" true "$r" apps/nodecraft/build.rs
check "Cargo.toml" true "$r" Cargo.toml
check "Cargo.lock" true "$r" Cargo.lock
check "clippy.toml defines the clippy gate" true "$r" clippy.toml
check "rustfmt.toml defines the fmt gate" true "$r" rustfmt.toml
check ".cargo/config.toml has the xtask alias" true "$r" .cargo/config.toml
check ".config/nextest.toml is a CI profile" true "$r" .config/nextest.toml
check ".gitattributes pins golden EOL" true "$r" .gitattributes
check ".gitignore" true "$r" .gitignore
check "assets/catalog is include_str!'d" true "$r" assets/catalog/comfyui.toml
check "the app icon is a build input" true "$r" assets/app-icon/nodecraft.svg
check "a WGSL shader" true "$r" assets/shaders/sky.wgsl
check "a golden fixture" true "$r" crates/testkit/tests/golden/baseline.json
check "README inside a fixture folder" true "$r" crates/format/tests/fixtures/v1/README.md
check "README in golden_driver" true "$r" crates/testkit/tests/golden_driver/README.md
check "a workflow change builds" true "$r" .github/workflows/ci.yml
check "the release workflow" true "$r" .github/workflows/release.yml
check "a composite action" true "$r" .github/actions/rust-leg/action.yml
check "dependabot config" true "$r" .github/dependabot.yml
check "a packaging script" true "$r" packaging/icons.sh
check "prose plus one .rs builds" true "$r" README.md crates/engine/src/lib.rs
# Markdown outside a listed prose root is not prose, so a new docs folder cannot whitelist itself.
put "$r" notes/architecture.md '# Notes'
put "$r" rust-notes.md '# Notes'
check "Markdown dropped in a new directory" true "$r" notes/architecture.md rust-notes.md

# --- The gate cannot pay for its own exemption. ---
g=$(repo gate) || exit 1
check "editing this script builds" true "$g" scripts/ci/rust-scope.sh
check "editing these tests builds" true "$g" scripts/ci/rust-scope.test.sh
# The real attack: ship a whitelist that whitelists everything, then use it to skip. CI runs the
# copy committed on the base revision, so this is exactly what happens when the base copy classifies
# the pull request: the new file is in the change set, it is under scripts/ci/, and it forces Rust
# even though the rewritten ruleset would have skipped it.
base_gate=$work/base-copy.sh
# The copy CI would run: the rules as committed on the base revision. Copied aside so the pull
# request's rewrite of the file in the checkout cannot be the copy that judges the pull request.
cp "$gate" "$base_gate" && chmod +x "$base_gate"
if grep -q 'is_prose()' "$base_gate"; then
    base=$(git -C "$g" rev-parse HEAD)
    # A pull request that replaces the rule with one that whitelists literally everything.
    printf '%s\n' '#!/usr/bin/env bash' 'set -u' 'is_prose() { return 0; }' 'is_whitelisted() { return 0; }' \
        'printf "rust=false\nbase=%s\n" "$(git merge-base "$BASE_SHA" "$HEAD_SHA")" >>"$GITHUB_OUTPUT"' \
        >"$g/scripts/ci/rust-scope.sh"
    chmod +x "$g/scripts/ci/rust-scope.sh"
    put "$g" docs/development.md 'prose'
    git -C "$g" add -A && git -C "$g" commit -qm 'rewrite the whitelist to skip everything'
    # Run the BASE copy against the pull request's change set, which is what CI does.
    decide "$g" "$base" HEAD "$base_gate"
    report "a permissive rewrite cannot skip the legs it governs" true "$decision" "rust=$decision" "$log"
    report "  it forced rather than merely declined" 1 "$(printf '%s' "$log" | grep -c 'the pipeline decides')" "forced=$(printf '%s' "$log" | grep -c 'the pipeline decides')"
    # And the rewritten copy, run on its own, would have skipped: proof the test discriminates.
    decide "$g" "$base" HEAD "$g/scripts/ci/rust-scope.sh"
    report "  the rewritten copy alone would have skipped" false "$decision" "rust=$decision" "$log"
else
    report "HEAD carries a copy of the gate to test against" ok missing \
        "run these tests from a checkout with the gate committed"
fi

# --- Deletions. ---
d=$(repo delete) || exit 1
remove "deleting a .rs is a build change" true "$d" crates/engine/src/lib.rs
remove "deleting docs is not" false "$d" docs/development.md
remove "deleting a workflow builds" true "$d" .github/workflows/release.yml
remove "deleting this script builds" true "$d" scripts/ci/rust-scope.test.sh

# --- The base branch has to be the protected one. ---
# `base.sha` is the tip of whatever a pull request targets, so a stacked pull request aimed at a
# colleague's feature branch would otherwise be classified by a draft copy of this whitelist. The
# same prose change set, four ways at the base branch.
b=$(repo baseref) || exit 1
prose_base=$(git -C "$b" rev-parse HEAD)
printf 'prose\n' >>"$b/docs/development.md"
git -C "$b" add -A && git -C "$b" commit -qm 'prose only'
decide "$b" "$prose_base" HEAD "$gate" main
report "prose aimed at main may skip" false "$decision" "rust=$decision" "$log"
decide "$b" "$prose_base" HEAD "$gate" feature/someones-branch
report "the same prose aimed at a feature branch builds" true "$decision" "rust=$decision" "$log"
decide "$b" "$prose_base" HEAD "$gate" ""
report "an unset base branch builds" true "$decision" "rust=$decision" "$log"
decide "$b" "$prose_base" HEAD "$gate" "mai"
report "  and a near-miss name is not main" true "$decision" "rust=$decision" "$log"

# --- Merge base, renames, and history the gate cannot read. ---
m=$(repo merge) || exit 1
git -C "$m" checkout -q main
printf 'teammate\n' >>"$m/crates/engine/src/lib.rs"
git -C "$m" add -A && git -C "$m" commit -qm 'teammate lands code on main'
git -C "$m" checkout -q -b feature main~1
printf 'prose\n' >>"$m/docs/development.md"
git -C "$m" add -A && git -C "$m" commit -qm 'this branch is prose only'
decide "$m" main HEAD
report "code a teammate landed on main is not this change's" false "$decision" "rust=$decision" "$log"
printf 'code\n' >>"$m/apps/nodecraft/src/main.rs"
git -C "$m" add -A && git -C "$m" commit -qm 'this branch adds code'
decide "$m" main HEAD
report "the same branch builds once it adds code" true "$decision" "rust=$decision" "$log"

mv_repo=$(repo rename) || exit 1
mv_base=$(git -C "$mv_repo" rev-parse HEAD)
git -C "$mv_repo" mv crates/engine/src/lib.rs notes.md
git -C "$mv_repo" commit -qm 'move a source file out of crates/'
decide "$mv_repo" "$mv_base" HEAD
report "a source file moved out of crates/ still builds" true "$decision" "rust=$decision" "$log"

mv2=$(repo rename2) || exit 1
mv2_base=$(git -C "$mv2" rev-parse HEAD)
git -C "$mv2" mv README.md docs/readme.md
git -C "$mv2" commit -qm 'move prose within the docs'
decide "$mv2" "$mv2_base" HEAD
report "a rename between two prose paths stays skipped" false "$decision" "rust=$decision" "$log"

# Criss-crossed history: two branches that each merged the other share two merge bases, so the
# answer depends on which one git picks and the gate declines to answer. The legs build.
x=$(repo criss) || exit 1
git -C "$x" checkout -q -b left main
printf 'left\n' >>"$x/docs/development.md"
git -C "$x" add -A && git -C "$x" commit -qm 'left commit'
left_sha=$(git -C "$x" rev-parse HEAD)
git -C "$x" checkout -q -b right main
printf 'right\n' >>"$x/ROADMAP.md"
git -C "$x" add -A && git -C "$x" commit -qm 'right commit'
right_sha=$(git -C "$x" rev-parse HEAD)
# Each side takes the other's *original* tip, so neither tip descends from the other and both
# share two bases: the two commits are peers rather than ancestor and descendant.
git -C "$x" checkout -q right
git -C "$x" merge -q --no-edit -m 'right takes left' "$left_sha"
tip_right=$(git -C "$x" rev-parse HEAD)
git -C "$x" checkout -q left
git -C "$x" merge -q --no-edit -m 'left takes the original right' "$right_sha"
# Both tips are prose-only against either base, so a gate that silently picked one would skip.
if [ "$(git -C "$x" merge-base --all "$tip_right" left | wc -l | tr -d ' ')" -gt 1 ]; then
    decide "$x" "$tip_right" left
    report "two merge bases is no answer, so it builds" true "$decision" "rust=$decision" "$log"
    report "  and it says why" 1 "$(printf '%s' "$log" | grep -c 'no single merge base')"
else
    report "the criss-cross fixture really has two merge bases" ok "one base" \
        "$(git -C "$x" merge-base --all "$tip_right" left)"
fi

e=$(repo edges) || exit 1
empty_base=$(git -C "$e" rev-parse HEAD)
# The empty commit that opens a claim (CONTRIBUTING.md) changes nothing: the base already passed.
git -C "$e" commit -q --allow-empty -m 'claim: #1'
decide "$e" "$empty_base" HEAD
report "an empty claim commit is an identical tree" false "$decision" "rust=$decision" "$log"

# Work git ignores (log/, plan/) is never staged, so it never forms a change set.
put "$e" log/2026-10-18.md 'notes'
if [ -z "$(git -C "$e" status --porcelain)" ]; then
    report "gitignored log/ work is not a change set" ok ok
else
    report "gitignored log/ work is not a change set" ok dirty
fi

u=$(repo unknown) || exit 1
decide "$u" HEAD 0000000000000000000000000000000000000000
report "an unknown head builds rather than skips" true "$decision" "rust=$decision"
report "an undecided gate still exits 0" 0 "$status"

decide "$u" "" HEAD
report "a missing BASE_SHA builds" true "$decision" "rust=$decision"
decide "$u" HEAD ""
report "a missing HEAD_SHA builds" true "$decision" "rust=$decision"

# An independent statement of the whitelist, so widening the rule in the script without saying so
# here fails. This list is the documented promise; the script must match it exactly.
eval "$(sed -n '/^is_prose()/,/^}/p' "$gate")"
if declare -F is_prose >/dev/null; then
    expected_prose() {
        case $1 in
        docs/*.md | README.md | ROADMAP.md | CONTRIBUTING.md | AGENTS.md | CLAUDE.md | ATTRIBUTION.md | LICENSE-MIT | LICENSE-APACHE | NOTICE | .github/*.md | .github/ISSUE_TEMPLATE/*.md | assets/*/README.md | packaging/release-notes.md) return 0 ;;
        *) return 1 ;;
        esac
    }
    mismatches=
    # NUL-delimited, because a filename with a space would otherwise be read as two paths and the
    # comparison would be nonsense.
    while IFS= read -r -d '' path; do
        if expected_prose "$path"; then
            is_prose "$path" || mismatches="$mismatches $path(should-skip)"
        else
            is_prose "$path" && mismatches="$mismatches $path(should-build)"
        fi
    done < <(git -C "$here/../.." ls-files -z)
    report "the script's prose roots match the documented list" ok "${mismatches:-ok}" "$mismatches"
else
    report "is_prose is still a function to test" ok missing
fi

# --- The wiring this safety depends on, asserted rather than commented. ---
# Every rule above is only worth anything if .github/workflows/ci.yml keeps four properties, and a
# later edit could break each one without touching this file. They are grep-level on purpose: a full
# YAML parse would need a parser the repo does not carry, and these are the claims that matter.
w="$here/../../.github/workflows/ci.yml"
if [ -f "$w" ]; then
    # 1. The three required contexts exist as literal job names, and no `matrix` is left to publish
    #    an unexpanded name.
    for nm in 'rust (ubuntu-24.04)' 'rust (windows-latest)' 'rust (macos-latest)' 'claim bot tests' \
        'rust scope' 'docs gate'; do
        if grep -qF "name: $nm" "$w"; then
            report "the check named '$nm' is published" ok ok
        else
            report "the check named '$nm' is published" ok missing
        fi
    done
    report "no job name still interpolates a matrix" 0 "$(grep -c 'name:.*matrix\.' "$w")"
    # 2. The classifier runs before the pull request's own test script, so nothing the pull request
    #    wrote can influence the recorded answer.
    classify_line=$(grep -n 'id: scope$' "$w" | head -1 | cut -d: -f1)
    tests_line=$(grep -n 'classifier tests' "$w" | head -1 | cut -d: -f1)
    if [ -n "$classify_line" ] && [ -n "$tests_line" ] && [ "$classify_line" -lt "$tests_line" ]; then
        report "the classifier runs before any pull request code" ok ok "line $classify_line < $tests_line"
    else
        report "the classifier runs before any pull request code" ok "id: scope at ${classify_line:-none}, tests at ${tests_line:-none}"
    fi
    # 3. That first run happens in a scrubbed environment, so a poisoned one is not inherited.
    # Matched on the command, not the phrase, which also appears in a comment above it.
    report "the classifier runs under env -i" 1 "$(grep -c '^[[:space:]]*env -i PATH=' "$w")"
    # 4. The legs ask for Rust unless a *green* gate said false. `!= 'false'` alone would let a job
    #    that reported false and then went red skip all three required checks.
    report "the legs require a successful gate" 3 "$(grep -c "needs.scope.result != 'success'" "$w")"
    report "  and still run on a missing output" 3 "$(grep -c "needs.scope.outputs.rust != 'false'" "$w")"
    # 5. Every job that needs scope says always(). Without it a failed gate silently skips its
    #    dependents, and a skipped required check never blocks a merge.
    unguarded=$(awk '
        /^[[:space:]]*needs: scope$/ { looking = 1; next }
        looking && /^[[:space:]]*#/ { next }
        looking {
            if ($0 !~ /always\(\)/) print $0
            looking = 0
        }
    ' "$w" | wc -l | tr -d ' ')
    report "  and nothing depends on scope without always()" 0 "$unguarded"
else
    report "the workflow to check wiring against is present" ok missing "$w"
fi

# --- Recording the answer. ---
# No output file: the consumer cannot see a decision, so the job must be red, not quietly green.
: >"$work/gho"
(cd "$u" && BASE_SHA=main HEAD_SHA=HEAD BASE_REF=main GITHUB_OUTPUT= bash "$gate" >/dev/null 2>&1)
noset=$?
if [ ! -s "$work/gho" ]; then
    report "no GITHUB_OUTPUT records no decision" ok ok
else
    report "no GITHUB_OUTPUT records no decision" ok "$(cat "$work/gho")"
fi
report "  and the job fails rather than passing silently" 1 "$noset" "exit=$noset"

# An output path that cannot be written (its directory does not exist) is the same case.
(cd "$u" && BASE_SHA=main HEAD_SHA=HEAD BASE_REF=main GITHUB_OUTPUT=$work/no/such/dir/out bash "$gate" >/dev/null 2>&1)
report "an unwritable GITHUB_OUTPUT fails the job" 1 "$?"
# A writable output keeps exactly the two keys the workflow reads, and nothing else can join them.
decide "$u" HEAD HEAD
report "the output is exactly rust and base" 'base|rust' "$(sed -e 's/=.*//' -e '/^$/d' "$work/gho" | sort -u | paste -sd'|' -)"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
