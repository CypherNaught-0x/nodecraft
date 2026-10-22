## 2026-10-23 · Bounded, indexed card option pickers (#678)

Fetched option lists keep host order and an index from stored value to row. The same index
serves membership checks and the selected label, so drawing a closed card does not scan its
list. A borrowed view exposes this index without copying the choices. Each accepted list
installation has a small shared identity token; replacing a list invalidates popup results
even when its length is unchanged. Retaining a token does not retain the old choices.

One active picker owns the filter state in egui view state. Queries are capped at 256 Unicode
characters. A nonempty query matches case-insensitively against labels and distinct stored
values, scanning at most 128 KiB of choice text and 1,024 choices per frame. Incomplete scans
request another frame and show progress. Results retain host order and use at most the
existing 16,384-option limit in row indices. Empty queries use the original row range directly.
This bounds filtering work without claiming a frame rate across hardware or workloads.

The popup keeps the card control's width, truncates long rows, virtualizes its scroll area and
starts at the current choice. Opening a different picker clears the query; refreshing the
same list keeps the query and rebuilds its results. Filtering changes only view state.
Choosing Default clears the stored override through the existing parameter edit, including
undo. It remains available for stale values and known-empty lists. Unknown lists retain the
existing editable text and Refresh guidance described in the option-source decisions.
