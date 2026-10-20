## 2026-10-20 · A batch wired into a list input can run once per item (#515)

- **The input says it fans out.** A list input collects every wire into one list, so a batch
  wired into Generate's `prompt` (a list, so several prompt pieces can be joined) ran once with
  every prompt joined. An input contract now carries `fans_out` (contract 1.2): a list input
  marked with it runs the node once per item of a batch wired into it, as a single input does.
  The generation kind marks `prompt`; its media roles keep collecting, because a batch of
  reference images is one request with several references.
- **The other wires join each item in place.** The planner keeps the fanned wire's position in
  the input's wire order, so each run sees its item where the batch was wired, between the
  pieces before and after it. Wire order still decides prompt order.
- **Several fanned wires combine as single inputs do.** Each fanning wire is a relation to
  `fanout::combine`, like a fanned single input: wires from the same batch join on its items,
  and different batches pair by their Cross or Zip setting. Two batches into one prompt
  therefore run every combination (Cross) or item by item (Zip), never silently joined.
- **Optional and refused where it can't apply.** `fans_out` defaults to false and is omitted, so
  older contracts mean the same; registration refuses it on a single port, where it would mean
  nothing.
