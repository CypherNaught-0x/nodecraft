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

A budget refusal happens before any provider call, so a job refused on its first attempt is
**Not charged** too. Estimates are labelled **est.** everywhere in the panel until the panel can
show settled charges.

**A cancel is a request, not a guarantee.** The run asks the provider to stop and waits briefly
for an answer. Only a confirmed cancel ends the job as **Cancelled** (and its card as cancelled,
not failed). If the provider refuses or does not answer, the job keeps being polled to its end
and shows **Cancel refused; still running (may be billed)**: the provider may still bill it, so
its result is kept and recorded rather than thrown away, and the job finishes as **Finished; the
provider refused to cancel**. A job whose result was already arriving when the cancel came in
keeps that result (**Finished before it could be cancelled**).

While running, a job distinguishes **Waiting at the provider** (queued there, with its position
when the provider gives one), **Generating** with its progress, and **Connection lost,
retrying** while polls fail transiently; a progress message is replaced, not kept, when the next
report has none.

The **Queue** toggle sits with **API keys** in the top-right row, left of the closed tag filter
and search, which own the corner; before this change the keys row overlapped them and a click on
it also opened search. The panel opens under search, since notices own the bottom-left corner and
the map the bottom-right.
