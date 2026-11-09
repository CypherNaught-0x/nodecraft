## 2026-11-09 · Escape closes the frontmost eligible floating tool (#959)

The question-or-tool rule distinguishes operations that must be answered from tools used beside
an editable canvas. Workflow fields and Batch items already received Escape before canvas input;
the other floating tools had only their close button and top-bar toggle. Extend that arbitration
to the registered tool inventory, including image, PDF and shader viewers, on ordinary and sheet
canvases. A tool must be open and actually drawn recently; an unavailable service cannot swallow
a key. The frontmost eligible panel layer wins, so one press closes one tool regardless of the
order its renderer runs. Canvas-only owners and tools do not participate on the sheet route;
their remembered open state is preserved for returning to the canvas.

The shell still gives questions, popups, text editors, quick-add, Turn into, keyboard connect,
Search, Discussions, a focused Tag filter, gestures and browser file reads their existing first
refusal. A focused notice keeps its earlier priority. An inline Techniques removal question also
keeps Escape for Keep. Shared parameters and Row settings remain pinned to their selection;
operation modals, the model picker and the Tag filter retain their own lifecycles. This is an
explicit inventory, not a global Escape handler for every window.

The shell sends a close request to the winning shared panel. The panel clears the same local
`open` flag as its header button, and the caller executes its existing cleanup. This matters for
forgetting typed API keys, dropping unapplied shader drafts and reader state, and clearing plugin
preview and consent. Requests last only for the current pass; a panel that did not render cannot
close a later reopening. Closing a tool creates no document change, undo entry or paid action.
Enter behavior is unchanged.
