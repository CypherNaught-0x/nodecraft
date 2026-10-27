## 2026-10-27 · Compatible card-body wiring targets (#951)

Keep precise input-port drops first, including their existing screen-space reach at overview
zoom. When no port is within that reach, dropping an output wire on a card uses the first free
compatible input in declared port order. A list input with room for another connection counts
as free; ignore the wire being moved when checking occupancy. If every compatible input is
occupied, choose the nearest compatible input by anchor distance, breaking ties by declared
order. This makes ordinary empty-card drops predictable while preserving a way to replace a
connection without having to hit a small port.

Use the same modality/list, self-connection, cycle and duplicate checks as precise wiring.
Only the top painted card under the pointer participates in body fallback; an incompatible
front card does not expose a card behind it. A candidate must be on actionable canvas, clear
of foreground overlays. Highlight the accepting hovered card using the existing theme tokens
at every zoom level. Retain the ordinary compatible-port rings and noodle preview.

Dispatch the existing `edge.connect` or `edge.rewire` command, including any displaced input
edge and the picked-up edge in the same changeset. Appending list order remains checked.
Undo restores the complete prior document. Refused card or overlay drops preserve a picked-up
wire; dropping it on empty canvas still disconnects it. Empty-canvas creation, Escape and
the reverse wiring path from an unconnected input retain their existing behavior.
