## 2026-10-18 · Stale badges, refresh and the app's run host (#211)

- **Stale badges come from `canvas.stale`, recomputed only when the canvas or revision
  changes.** A card whose result is stale shows a badge in its title row, above the card and clear
  of its controls. The badge uses the `stale` theme token and is labelled "Stale result: refresh"
  for assistive tech. The card draws it last, in the card's layer, so the badge, not the card, takes
  the click. A failing stale check is reported once.
- **A refresh is a run of the node with what it needs upstream:** `run.quote` in
  `with_upstream` mode, then `run.start` with the quote's token. A free, fully priced refresh
  runs at once. Anything else first asks the user to confirm the price, noting when part of it
  isn't known.
  - An edit closes an open confirmation, because its price no longer holds.
  - While the confirmation is open it blocks canvas input (clipboard, quick-add).
  - A refresh that would call a model with no provider set up says so, instead of asking for
    money.
  - A budget block or an unplannable request becomes a notice.
- **The app hosts `run.start` for every caller:** the badge, the palette, the control channel and
  agents. It takes a started run from its mailbox every frame and:
  1. prices it again (`run_cmds::price`) and refuses it if the token changed or the budgets no
     longer allow it;
  2. runs exactly the plan that was priced, on the canvas it was priced for, with exec's run
     executor;
  3. records the results with `run.record`, as one undo step.

  A run is limited to `MAX_RESULTS` (1,000) steps, so its results always fit one record. Jobs that
  need no provider run inline. Model calls fail clearly until a provider registry and an async
  runtime exist (#201, part 2).
- **Run ids mix every generation id already on the canvas, a counter and per-process randomness**
  (where the platform provides it). Generation ids derived from them don't repeat within a
  session, nor across restarts on native builds.
