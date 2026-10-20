## 2026-10-20 · Stale badges hash once per revision (#211)

A card whose stored result no longer matches its inputs shows a **Stale ↻** badge under its
bottom-right corner, in the `stale` theme token. The set comes from `canvas.stale`, asked once
per session, canvas and document revision and kept until one of those changes. This refines the
#339 record, where nothing outside the explicit diagnostic hashed inputs: a revision is the only
trigger, never a repaint, pan, hover or search. A failed check shows no badges rather than
guessing, and at most 4096 badges are kept.

Clicking the badge quotes a refresh (`run.quote`, mode `with_upstream`) of that card, and the
person confirms the price exactly as for Generate; the modal is titled **Refresh** and its
button **Refresh for …**. Before quoting, the desktop sets up the provider of every card the
refresh will run: the card, and upstream cards that are stale or have no result, walked as the
planner walks them, so a fresh ancestor stops the walk and its provider is never needed. Every
provider that can't run is named at once. A card that is running shows no badge, so a refresh
can't be started twice, and a badge click while any overlay owns the canvas does nothing.

An earlier attempt (PR #301) ran its own run host in the shell; runs now go through the engine
run host (#409) and the desktop's `Runs`, and this change reuses them.
