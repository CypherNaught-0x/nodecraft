## 2026-11-04 · Run jobs are listed and cancelled through commands, agents included (#167)

**Problem.** #167 asks that training be followed and cancelled through stable engine commands,
which MCP can expose. A job's state lived only in the run host, and cancelling meant calling
`RunHost::cancel` from the queue panel. An agent could start a paid training with `run.start`,
but it couldn't see the training's progress or stop it.

**Decision.**

- **`run.jobs` and `run.cancel`** (`run_job_cmds`) follow the `export.status` and `export.cancel`
  pattern. Each is a command over a host service (`run.jobs_host`) that the run host provides.
  - `run.jobs` lists jobs as of the host's last frame, newest last, at most 256.
  - `run.cancel` queues a cancel that the next frame carries out, exactly as the queue panel's
    button does.
  - Neither opens a dialog.
- **A cancel is no refund.** The answer says `may_be_billed` for a job already sent.
  - A provider that refuses to cancel, or can't, keeps its job followed to its end, and
    `run.jobs` reports `cancel_refused` with the status the queue panel shows.
  - A job not yet sent is never sent.
- **Losing a paid result takes an explicit yes.** A job that was already sent is cancelled only
  with `accept_loss: true`. Without it, the refusal says the result may be billed and lost, and
  that getting it again means paying again. A job not yet sent needs nothing: nothing can be
  lost. This matters because external MCP agents have no approval step of their own. The
  contrast is `run.held_discard` (#369), which lets go of a result already in hand and stays
  human-only. A cancel stops spending on a result not yet received, so an agent may make it, but
  only by saying in its own request that it accepts the loss.
- **`may_be_billed` alongside `may_have_been_charged`.** `may_have_been_charged` means "ended
  without a kept result and may still be billed". It is false for a job that is running or done.
  `may_be_billed` is true for any job that reached the provider and wasn't proven refused, so an
  agent tallying spend counts those.
- **Agent-callable, both.** Neither spends money, and stopping work can only lower what a run
  costs. Agents may already start paid runs through `run.start`, so an agent that can start one
  must be able to stop it. The one cost of a cancel is a result that is already paid for but not
  yet fetched, and it is the same cost the person takes with the queue panel. We judged that
  smaller than an agent being unable to stop a runaway paid run.
