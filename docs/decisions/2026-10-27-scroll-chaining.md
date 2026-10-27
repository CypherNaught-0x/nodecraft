## 2026-10-27 · Scroll chains from card regions to the canvas (#413)

Two-finger scroll and the mouse wheel pan the canvas everywhere, including over cards. Being over a
card used to switch canvas scroll off, which made every card a dead zone at readable zoom. The rule
is now *consumed and not at a limit*, not *is a card*: the innermost scrollable region under the
pointer takes the part of the delta it can use before it hits its limit in that direction, the
region around it gets what is left, and the canvas pans (or, with Scroll = Zoom, zooms) by the
remainder. Horizontal scroll over a vertical body goes straight to the canvas.

A gesture belongs to whoever took its first delta (`ui-egui/src/scroll_chain.rs`, `Gesture`). One
that started on the canvas hides its delta from card regions while the pointer crosses them, so a
swipe never stutters or stops mid-way; one that started in a body keeps chaining from there. The
gesture ends when egui stops reporting scrolling (the trackpad's end phase, or 150 ms after wheel
notches).

Mechanics others should reuse:

- egui only lets a `ScrollArea` take the wheel on the layer it finds under the pointer, and a card's
  transformed sub-layer is never that layer. A scroll area on a card therefore calls
  `scroll_chain::take` after `show`, with whether the card layer's hit test has the pointer over it.
  `take` moves the area's stored offset and writes the remainder back into
  `smooth_scroll_delta`, as egui's own areas do. A future scrollable node (media or document
  previews, long logs) does the same; anything that uses scroll itself, like Alt-scroll in the 3D
  viewer, clears the part it used.
- The canvas applies the remainder after the card pass, as it already did for drag pan that starts
  on a card, so card scroll and canvas scroll are measured on the same frame. The canvas moves one
  frame after the input, like card-drag pan; pinch and ⌘/Ctrl-scroll zoom still apply before the
  cards are drawn.
