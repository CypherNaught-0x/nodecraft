## 2026-10-20 · Text answers stream into the card as view state (#504)

**A text job streams when the runner asks for it.** The run host's runner sets
`RunLimits::stream_text`; an LLM adapter then asks the provider for a streamed answer and reports
each piece through the runner as `RunUpdate::Text`, while the finished answer still arrives whole.
Pieces are bounded (64 KiB each) and the total per submit is capped (256 KiB), always at char
boundaries. Followed jobs after a restart (#508) don't stream: they are polled, not sent.

**What streamed is view state, never a document change.** The host folds the pieces into the
job's view (`JobView::partial`), which the card shows in place of its answer while it arrives.
Only the finished answer is recorded with `run.record`, so undo holds one result per run. A retry
starts the answer afresh, and a finished job drops what streamed.

**A cut-short answer is kept only when the person keeps it.** A job that is cancelled or fails
keeps what streamed in its view, and the card offers Keep and Discard. Keeping records it with
`run.record` and `keep: true`: it joins the card's history as one undo step but does not become
the shown answer, unless the card had none. An error or a partial answer never changes the active
selection.

**A text card always has room for its answer**, with a placeholder until the first one, so the
card doesn't jump when an answer starts streaming in. This replaces #84's card that grew once it
had an answer.
