## 2026-11-08 · Notices are typed, stacked and dismissible (#1110)

The shell had one notice slot, `notice: Option<String>`, for errors and successes alike: drawn
the same, silently replaced by the next message, with no dismiss and no live region. It is now
`notices: Notices` (`crates/ui-egui/src/notices.rs`), view state like the rest of the shell.

- A notice has a kind, **Info**, **Success**, **Warning** or **Error**, its text (at most 4 KiB),
  an optional action (a command id and a label) and `sticky`, which errors have, and run warnings
  (`Notice::keep`). The stack shows three; a new one replaces the oldest that isn't sticky. With
  only sticky ones shown, a passing notice is dropped rather than evict one, and a further sticky
  one is kept (at most 32) and counted on a "more" line under the three. Repeating a message
  refreshes it.
- Sticky notices stay until dismissed; the others time out after 6 s on screen in egui time (so
  tests can step it), counting the whole gap between frames of an idle window, paused under the
  pointer, while a notice's button has the focus, and while the stack isn't drawn.
- Escape dismisses a notice only while it holds the focus. egui drops the focus on the press, so
  the stack remembers the last frame's focused notice and the shell takes the key with the tools
  in `take_tool_escape`, after every other Escape owner.
- The kind is chosen where the message is made. Engine errors go through `Notices::report`, which
  maps `ErrorKind` in one place (`NoticeKind::of_error`: `Busy` is a warning, everything else an
  error, since runs cost money); run events map in `NoticeKind::of_run`; background workers'
  one-line reports (option lists, ComfyUI nodes, templates, update checks) through
  `NoticeKind::of_report`, which reads their own wording.
- `notices.clear()` (what `notice = None` became, called after many unrelated successes) clears
  only passing notices. It never removes a sticky one: a "The run didn't start: <budget>" error
  shown on no card would otherwise vanish when the person pressed Delete. Code that wants to take
  back its own earlier notice does it by id (`report_id`, `push_notice` return it) or text
  (`dismiss_text`), as the fit's minimum-zoom warning does; otherwise the person dismisses it.

Panel-local messages (API keys, Budgets, Plugins, Provider servers) keep their own lines inside
their panels; they are not part of the stack.
