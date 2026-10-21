## 2026-10-21 · The in-canvas agent judges each call before making it (#114)

- **The loop is sans-IO.** `nodecraft_agent::Agent` returns the next chat request with a ticket
  and takes the reply back; the host sends it on whatever thread or task it has. A reply with a
  ticket that is no longer current (after cancel) is ignored, so a late reply never acts.
- **Tools are the MCP tools.** The agent sees `command_list`, `command_run`, `canvas_inspect` and
  `ids_new` through `nodecraft_automation::tools`, so the in-canvas agent and external MCP clients
  are held to one classification (`AGENT_COMMANDS`, `HUMAN_ONLY`) and one dispatch path.
- **Impact comes from the handler, not from a list.** `Session::impact` describes a command as
  dispatch would and classifies the effect: read, document edit, host request, or run with the
  ceiling it accepts. It refuses what dispatch would refuse, by preparing an edit and dropping
  it. A new command needs no extra classification to be safe in assist mode.
- **Calls are judged one at a time, just before each runs**, because an earlier call in the same
  reply can make a later one possible (add a node, then connect it).
- **Assist asks before anything that isn't a read.** Approving covers the rest of that reply, but
  only calls that still do exactly what was shown: a call that couldn't be judged yet, or now
  would do something else (a run priced once an earlier edit lands), is judged again and may
  need its own approval. Declining sends an error result for each remaining call, so the model
  can ask what to do.
- **Auto spends only what it can bound.** A run goes ahead without asking only if it is fully
  priced (not partial, no unpriced jobs) and its ceiling fits what is left of the session budget
  in every currency; the ceiling is what is charged against it. The engine still enforces the
  person's budgets on every run.
- **The conversation stays valid.** Every tool call gets a result (cancelled, declined, over the
  per-reply limit), results and errors are cut to 32 KiB at a character boundary, blank or malformed replies
  are reported instead of kept, and old turns are dropped whole, at a question, once the
  conversation passes 400 messages or 2 MiB, so a call never loses its result.
