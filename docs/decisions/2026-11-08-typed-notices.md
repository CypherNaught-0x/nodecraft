## 2026-11-08 · Notices are typed, stacked and dismissible (#1110)

The shell had one notice slot, `notice: Option<String>`, for errors and successes alike: drawn
the same, silently replaced by the next message, with no dismiss and no live region. It is now
`notices: Notices` (`crates/ui-egui/src/notices.rs`), view state like the rest of the shell.

- A notice has a kind, **Info**, **Success**, **Warning** or **Error**, its text (at most 4 KiB),
  an optional action (a command id and a label) and `sticky`, which errors have. The stack keeps
  three; a new one replaces the oldest that isn't an error, and repeating a message refreshes it.
- Errors stay until dismissed; the others time out after 6 s on screen in egui time (so tests can
  step it), paused under the pointer or while a notice's button has the focus.
- Escape dismisses a notice only while it holds the focus. egui drops the focus on the press, so
  the stack remembers the last frame's focused notice and the shell takes the key with the tools
  in `take_tool_escape`, after every other Escape owner.
- The kind is chosen where the message is made. Engine errors go through `Notices::report`, which
  maps `ErrorKind` in one place (`NoticeKind::of_error`: `Busy` is a warning, everything else an
  error, since runs cost money); run events map in `NoticeKind::of_run`; background workers'
  one-line reports (option lists, ComfyUI nodes, templates, update checks) through
  `NoticeKind::of_report`, which reads their own wording.
- `notices.clear()` keeps the old "a later success clears the error" behaviour of
  `notice = None`, so an action that succeeds after failing doesn't leave its stale error up.

Panel-local messages (API keys, Budgets, Plugins, Provider servers) keep their own lines inside
their panels; they are not part of the stack.
