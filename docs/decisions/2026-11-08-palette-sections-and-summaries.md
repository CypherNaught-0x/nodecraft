## 2026-11-08 · Quick-add palette: sections, summaries and unavailable commands (#1121)

The quick-add palette was one alphabetical list of kinds and commands with nothing to tell
"3D photo" from "3D view sheet", and it hid every command that couldn't run.

**Summaries are metadata of the kind or command.** `NodeDescriptor::summary()`
(`crates/nodes/src/summary.rs`) and `CommandSpec::summary()` (`crates/engine/src/command_summary.rs`)
return one line each, from one table per crate keyed by id. A field on `NodeDescriptor` and
`CommandSpec` was the alternative; it would have touched some 60 descriptor and 240 command literals
across crates for text only the palette reads today. `params_doc` stays the agent-facing
description; it is written in shapes and edge cases, not for a person scanning a list. Tests
require a summary for every built-in kind and every command that needs no parameters (or only the
canvas the palette supplies). A plugin's kind has none; the palette shows its ports ("Image →
Image").

**Sections.** With nothing typed: Recent, then node kinds by the media they make (Text, Image,
Video, Audio, 3D, Document, Other), Actions (grouped by the media they work on), then Commands. A
row's dot is its modality's `wire_*` colour. Typing ranks every entry together with a small
deterministic scorer (whole label, label prefix, word prefix, inside the label, inside the id,
three or more letters in order, inside the summary; shorter labels first within a tier), bounded
by the 128-character query and 256 label characters, with the section named on the right.

**Unavailable commands are listed, greyed, after the runnable ones**, with the `enabled` error as
their description; Enter or a click does nothing. Only errors that say why a command can't run
*now* qualify (`Conflict`, `Busy`, `Forbidden`, `Budget`, `Authentication`): "Nothing to undo",
"Performance capture isn't running". `InvalidInput` and `NotFound` mean the command needs
parameters the palette can't give, and `Unsupported` means this host can't run it at all (the CLI
or browser for a desktop-only command); those stay out, as before. Only commands whose schema requires
nothing beyond the parameters the palette supplies (none, or the active canvas) are listed at
all: many edits check the role or the recovery state before their parameters, so in a view-only
share or a recovery that needs saving "Add node" would otherwise show as greyed.

**Recent is view state.** The last five entries chosen and run (a refused choice isn't kept) live in egui's temporary memory for the
window: never in the document, not in settings, and gone on restart. Persisting it would need a
settings key and a migration for little gain; reopen that if people ask for it.

**Placement.** The popup opens at the double-click when it fits below; otherwise upward from it
when there is more room above, else below with its list scrolling. It is kept inside the window
less `navigation_margin`, so it never runs off the bottom or right edge.
