## 2026-11-08 · Notices are typed, stacked and dismissible (#1110)

The shell had one notice slot, `notice: Option<String>`, for errors and successes alike: drawn
the same, silently replaced by the next message, with no dismiss and no live region. It is now
`notices: Notices` (`crates/ui-egui/src/notices.rs`), view state like the rest of the shell.

- A notice has a kind, **Info**, **Success**, **Warning** or **Error**, its text (at most 4 KiB),
  an optional action (a command id and a label) and `sticky`, which errors have, and run warnings
  (`Notice::from_run`, `Notice::keep`). The stack shows the newest three. A new notice always
  goes on top and is shown; room is made by dropping the oldest passing notice only, never a
  sticky one, and the notices it covers are counted on a "more" line under the three. Sticky
  notices are bounded at 32, dropping the oldest that isn't about a run first. Repeating a
  message refreshes it.
- Sticky notices stay until dismissed; the others time out after 6 s on screen in egui time (so
  tests can step it), paused under the pointer, while a notice's button has the focus, and while
  the stack isn't drawn. A frame counts as at most 0.5 s and the stack asks for a repaint at
  least that often while a notice is timing out, so an idle window still clears it in about
  6 s, a backgrounded one coming back doesn't expire everything at once, and a notice is never
  aged in the pass it was pushed.
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
  back its own earlier notice raises it with an owner (`report_for`, `error_for`) and calls
  `settle(owner)` when the same thing later succeeds, or uses the id (`report_id`) or text
  (`dismiss_text`), as the fit's minimum-zoom warning does. The common self-correcting errors do:
  a card's edit (owner `<command> <card>`), a connection (`wire`), a save (`file.save`), an export
  (`export`), and an id that couldn't be allocated or a paste with no canvas space
  (`canvas.add`, settled by the next document change). Anything else stays until dismissed.

Panel-local messages (API keys, Budgets, Plugins, Provider servers) keep their own lines inside
their panels; they are not part of the stack.
