## 2026-10-21 · Paid results a run can't keep are held, not dropped (#369)

- **What is held.** A finished model job's result is held, exactly as the provider returned it,
  when it can't be converted or imported, doesn't fit its card's ports, or can't be recorded. A
  text card given two texts, or an image card given a text, are examples; so is a card that was
  deleted while its job ran. The card's failure says the result is held. Before #369 these
  results were charged and then lost.
- **Never part of a result.** The host checks the whole result against the node's ports
  (`Run::admit_outputs`) before importing anything. A list port takes up to `MAX_LIST_ITEMS`
  values, a scalar port one, and an unknown port none. So a result is recorded whole or held
  whole, never trimmed.
- **Three ways out, none of which calls the provider again.**
  - `run.held_retry` records the result on its card as its step would have. It is offered only
    when the step was this job's alone.
  - `run.held_place` puts each output on the canvas as a source node, in order, to the right of
    its card, as one undo step.
  - `run.held_discard` lets the result go; its charge stands.
  The queue panel lists held results with these three buttons, and `run.held` lists them for
  agents.
- **A result stays held until its action works.** Retry and place run on the host's next frame.
  If one fails (for example the card is still gone), the card reports why and the result is kept.
  While an action is pending, the result refuses any other action.
- **Bounded by refusing new work, never by dropping.** At most `MAX_HELD` (16) results are held.
  While that many wait, every new job is refused before it is sent, so nothing more is charged.
  Jobs already in flight may still add theirs, so the store can briefly exceed the bound, but
  only by the number of in-flight jobs.
- **Memory only.** Held results are lost when NodeCraft quits; the queue panel says so. Writing
  them to the run journal is a follow-up (#611).
- **Agents may retry and place, not discard.** Discarding a paid result can't be undone, so MCP
  lists `run.held_discard` as human-only.
- **Follow-up (#611): multi-job steps.** A fanned or multi-job step whose record fails after every job
  finished is not held yet. Neither are the results of sibling jobs that are lost when one job of
  the step fails. Each job's own conversion failures are held.
