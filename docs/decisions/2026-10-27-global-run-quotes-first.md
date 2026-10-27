## 2026-10-27 · Global ⌘/Ctrl+Enter quotes before it runs; Run workflow (#965)

⌘/Ctrl+Enter outside a text field runs **Run changed**, and the palette also offers **Run
workflow**. ComfyUI's ⌘/Ctrl+Enter queues work at once. NodeCraft does not do that. Generative
runs cost money and give different results each time, so the plan ([§2.3](../plan.md)) makes
them explicit: priced first, started only after the person confirms. A global key that started
paid work without asking would break that rule. Both actions therefore open the same price
confirmation as a card's Generate. The dialog shows the cards that will run, the total, anything
blocking the run and any part of the price that isn't known. Cancel starts nothing. The owner is
asked to confirm this difference from ComfyUI on the PR.

**What runs.** Two `run.quote` modes in the planner choose the nodes:

- `pending` (Run changed): every generation, action and technique node whose result is stale,
  part-made or missing, with the upstream it needs, in dependency order. It replaces `all_stale`
  for the shortcut, because `all_stale` leaves out a card that has never run unless a stale card
  needs it.
- `workflow` (Run workflow): every generation and technique node, fresh or not, plus everything
  `pending` would run. "Every generative node" means model work. A fresh action keeps its result
  and recomputes by itself when its inputs change, as before.

In both modes, notes, decks, routers, batches and sources are not steps of their own. A node
whose kind isn't installed counts as work, so it blocks the run with "install its definition"
and isn't skipped silently. The whole canvas is planned together. One blocked card therefore
blocks the run, and the dialog names it, the way ComfyUI refuses to queue a graph with an
invalid node. Cached results (#91) are reused as in any other run.

**Commands.** `run.changed` and `run.workflow` take `{canvas}` and return the `run.quote` result
for their mode, plus `mode`. They are read-only and start nothing. The run starts through
`run.start` with that mode, the quote's token and its totals, so a stale or repriced quote is
refused. Each mode has its own token binding. The palette, the shortcut, the CLI, the control
channel and MCP agents all reach the same commands, with the same labels. The shortcut is
declared on `run.changed` (`Command+Enter`).

**Keys.** A focused text field keeps its own ⌘/Ctrl+Enter: a prompt quotes its own card, and
multiline text commits. A drag, an open menu, the bulk panel and every modal also keep the key.
⌘/Ctrl+Enter on a focused (not editing) card runs the global Run changed. Plain Enter still
generates that card. When everything is up to date, the shortcut opens no dialog and says
"Nothing to run".
