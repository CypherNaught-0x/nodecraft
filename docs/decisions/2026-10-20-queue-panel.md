## 2026-10-20 · The queue panel follows every job in the engine run host (#82)

The engine run host (#409) keeps a view of every job it runs: its card, provider, model,
estimate and state (queued, running with its provider position and progress, retrying, done,
failed with a cause, cancelled), fed by the scheduler's events and the providers' progress
reports. Ended jobs move to a bounded list of the 64 most recent; provider text in a view is
bounded and a progress fraction outside 0–1 is ignored. `RunHost::jobs`, `cancel` (one job) and
`clear_finished` are what any front end needs; the desktop's **Queue** panel is the first.

A job counts as **sent** once it has started at a provider, not when the provider acknowledges
it, because a request may be in flight. A cancelled job that was never sent shows **Not
charged**; one that was sent, and a failed one, show its estimate as **unresolved**, never as
refunded, since only the provider knows whether it billed. A failure names its cause from the
provider error's kind (`ErrorKind::label`, which is also how the error displays), so the card and
the panel can say **Run failed: API key** and hint at the fix. Retry in the panel quotes the
card again, exactly like Generate, so a retry never spends without showing its price.

The **Queue** toggle sits with **API keys** in the top-right row, left of the closed tag filter
and search, which own the corner; before this change the keys row overlapped them and a click on
it also opened search. The panel opens under search, since notices own the bottom-left corner and
the map the bottom-right.
