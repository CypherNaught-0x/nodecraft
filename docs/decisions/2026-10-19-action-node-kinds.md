## 2026-10-19 · Action node kinds run as local `Action` jobs (#410)

- **Kinds declare, the engine executes.** `nodes::action` registers the action kinds (8 image, 3
  text) with ports and parameter schemas only; `nodes` doesn't depend on `actions`. Planning emits a
  new `JobSpec::Action { action, params, inputs, output }` with the node's visible values and the
  values on its input port. `engine::action_exec::LocalActions` maps those values onto the
  `nodecraft-actions` requests and runs them.
- **Local, free, unrouted.** Action jobs run on exec's local lane through `SourceRunner`, which now
  takes an `ActionExecutor` from the host. They need no provider binding, admission prices them at
  nothing, and failures are ordinary job failures with a message.
- **New images are staged, not stored.** An image action encodes its result as PNG into a staging
  store and returns its asset reference; the host imports the staged assets
  (`LocalActions::take_staged`) before `run.record`, so a recorded result never names an asset the
  project doesn't hold. Inputs are read from a clone of the session's store (shared buffers).
- **Schemas are the first cut**, mapped by hand onto each operation's options (Blur is Gaussian
  only; Crop is normalized; Resize scales). The #162 parameter contract will refine them; the
  remaining operations (key, replace colour, aspect, overlay, duplicate, template) follow.
