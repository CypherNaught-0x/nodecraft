## 2026-10-17 · Chat turns, tools and usage (#195)

- **Finished jobs report usage.** `Submission::Finished` and `JobProgress::Done` carry a
  `Completed { outputs, usage }`, where `Usage` holds token counts and a cost in `types::Money`
  when the provider reports one. That cost is the job's actual cost for budgets; without it,
  budgets charge the estimate. A reported USD float is rounded up to the micro-dollar, so a
  charge is never free; NaN, negative and absurd amounts are ignored.
- **One conversation model for both dialects.** Tool calls, tool results and dialect-specific
  blocks (`Part::Opaque`) are parts of messages. Replies come back as an assistant `Message` to
  append unchanged: Claude's thinking blocks are bound to the conversation and must be resent
  as they came. Each dialect gets only its own opaque blocks.
- **Tools are `auto` only**, with optional `strict`. Forced tool choice is not offered: current
  Claude models return 400 for it.
- **Conversations are checked before sending**: calls are answered in the next message, results
  answer the previous one, tool names and schemas are valid, and requests are capped at 128 tools
  and 1,024 reply blocks. A malformed history fails with a clear error before anything is billed.

