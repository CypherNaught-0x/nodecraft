## 2026-11-08 · Dialogs are questions (modals) or tools (floating panels) (#1116)

The shell had three dialog systems: `egui::Modal` for questions such as the recovery offer and the
run quote, `floating::panel` for tools (#957), and ten raw `egui::Window`s that skipped the panel
area's clamping, cascade and stacking. Two of those windows asked a question without being modal.

The rule, stated in [`ui-design.md`](../ui-design.md#overlays-and-stacking):

- A **question** the person answers before continuing is `floating::question`: an `egui::Modal`
  in the shared panel frame (`theme::panel_frame`), no wider or taller than the window less its
  margins. The caller reads `floating::answer_keys` before showing it, so Enter takes the primary
  action and Escape cancels; both keys are consumed, and the question is drawn before any canvas
  handler, so neither reaches the canvas. Its area is an AccessKit `Dialog` named by its title.
- A **tool** the person works alongside is a `floating::panel`. The fixed Shared parameters panel
  is `floating::pinned`, which keeps its pinned place.
- Nothing outside `floating.rs` builds an `egui::Window`. `floating::panel` and
  `floating::pinned` wrap one internally; `floating::tests::no_raw_windows_outside_floating`
  fails on a new call site in non-test code.

The ten windows were sorted like this:

| Window | Now | Why |
|---|---|---|
| Review model change, Model preview changed | question | they decide whether the model change is applied |
| Rename field | question | the canvas waits for the name; Enter saves |
| Unsaved changes (browser) | question | opening the picked project waits for it; Enter discards and opens |
| Choose a model | panel | searched and browsed; it stays in front of the panel layer while open, because the canvas waits for the choice |
| Model choice unavailable | inside the Choose a model panel | it reported a failed choice beside the picker; it is now a line at the top of the picker |
| Workflow fields, Batch items | panel | worked in beside the canvas |
| Project files | panel | a menu-like tool; it opens below the File button (`Home::LeftBelow`) and is short enough not to be clamped over it, until #1123 makes it a menu |
| Shared parameters (and a sheet's Row settings) | pinned | fixed beside the selection it edits; it goes with the selection |

Enter on Unsaved changes discards the current project's edits. It is the question's only action
besides Cancel, so the rule applies to it too; the dialog says what will be lost.

Tools that are drawn late in the frame (Workflow fields, Batch items) take their Escape early:
the shell consumes it for the frontmost open one before any canvas handler runs, unless a
question, a popup or a text field owns the key (Escape leaves a text field first). Project files
and the model picker read their keys at the top of the frame already. `floating::on_screen`
(#1115) had no callers left and was removed.
