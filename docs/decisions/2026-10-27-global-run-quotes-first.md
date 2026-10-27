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
whose kind isn't installed and has no current result counts as work, so it blocks the run with
"install its definition" and isn't skipped silently; one holding a fresh result is left alone.
Any node with work that sits between two nodes of the run runs too, in order. Without that, a
fresh action between two generators would feed the later one a result made from the earlier
one's old output. A batch collects existing results only (#92), so everything after a batch
that the run feeds waits: the quote lists it in `waiting`, the dialog says so, and the next
Run changed picks it up once the results exist (it is then stale or missing). This keeps a
canvas with a batch between generators runnable instead of blocked. The whole canvas is planned
together. One blocked card therefore
blocks the run, and the dialog names it, the way ComfyUI refuses to queue a graph with an
invalid node. Cached results (#91) are reused as in any other run.

**Commands.** `run.changed` and `run.workflow` take `{canvas}` and return the `run.quote` result
for their mode, plus `mode`. They are read-only and start nothing. The run starts through
`run.start` with that mode, the quote's token and its totals, so a stale or repriced quote is
refused. Each mode has its own token binding. The palette, the shortcut, the control channel and
MCP agents (`command_run`) all reach the same commands, with the same labels. The
`nodecraft-cli quote` and `run` subcommands are not wired to these modes yet; they still quote
named cards or `all_stale`. The shortcut is
declared on `run.changed` (`Command+Enter`).

**Keys.** A focused text field keeps its own ⌘/Ctrl+Enter: a prompt quotes its own card, and
multiline text commits. The key also yields to everything that holds the canvas's letter keys
(drags of cards, noodles, fields, comments and the minimap, an open menu or popup, the palette,
Turn into, the bulk panel and the dialogs that block the canvas), to the API keys and provider
panels, and to the recovery question at startup, whose answer may replace the document.
⌘/Ctrl+Enter on a focused (not editing) card runs the global Run changed. Plain Enter still
generates that card. When everything is up to date, the shortcut opens no dialog and says
"Nothing to run". A card still running from an earlier run is not started again: the dialog
names how many are still running and its start button stays off until they finish and the run
is quoted again, so nothing is generated and charged twice.

Free actions keep recomputing by themselves without a quote (#88); that is covered by
`app::live::tests::an_edit_recomputes_its_action_and_one_undo_takes_both_back`.
