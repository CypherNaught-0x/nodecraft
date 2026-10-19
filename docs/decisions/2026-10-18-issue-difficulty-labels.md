## 2026-10-18 · Issue difficulty labels (#416)

- **Every open issue carries exactly one `difficulty:*` label** — `easy`, `medium` or `hard` —
  alongside its `area:*`. Area labels say *where* the work is; difficulty says *what signing up
  costs*, the first question a contributor asks and one `area:*` never answered. The levels and
  tie-breakers are defined once in [`../issue-labels.md`](../issue-labels.md); this record fixes
  only why they are rated the way they are.
- **It rates implementation difficulty, not priority.** Urgency belongs to the milestone and
  `future` where those are set, and importance is a maintainer judgement. Difficulty is "how much
  design work is left for someone who has read [`../../AGENTS.md`](../../AGENTS.md) and the docs
  for that area", which stays useful to contributors and goes stale far more slowly than a
  priority order.
- **Shape, not size, and ties go up.** The two rules most likely to be quietly undone are written
  down: novelty outranks line count (a 20-line change to scheduler fairness is `hard`, a large
  mechanical refactor on a settled pattern is `medium`), and "the parts exist, nobody wired them"
  is `medium` — crossing the `ui-egui`/apps/providers seam *is* the work.
- **`good first issue` stays independent.** It also promises onboarding value, so an easy issue is
  not automatically a good first one. It is a floor: nothing carrying it is rated higher.
- **Re-rate when the shape changes, not on a schedule.** A spike that settles a design question
  makes the follow-on cheaper; a discovered dependency makes it dearer. The label is a claim aid,
  and one that quietly lies is worse than none.
- **Applied to every open issue on 2026-10-18** in one pass; new issues get theirs in the same
  triage as their `area:*`. Enforcement at creation and claim time is tracked in #420.
