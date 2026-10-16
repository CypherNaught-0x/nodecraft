# UI design

> The tokens live in [`crates/ui-egui/src/theme.rs`](../crates/ui-egui/src/theme.rs). Change the
> code and this page together.

## The look

A dark, quiet canvas that lets media carry the colour (plan §1.2): near-black background, a fine
dot grid, dark cards with large radii and a hairline border, a small uppercase monospace label
above each card, and one round `+` output handle on the card's right edge. Selection brightens the
border instead of adding colour. Floating chrome (zoom badge, later the left rail, toolbars and
menus) uses a slightly lighter panel surface with the same hairline border. A light theme uses the
same structure with the values inverted.

## Tokens

| Token | Dark | Light | Use |
|---|---|---|---|
| `canvas_bg` | `#080809` | `#f4f4f5` | infinite canvas |
| `grid_dot` | `#2a2a2e` | `#cfcfd4` | grid dots (contrast with the canvas held between 1.2 and 3 by a test) |
| `panel_fill` / `panel_stroke` | `#171719` / `#2c2c30` | `#ffffff` / `#dddde1` | floating chrome, menus, windows |
| `card_fill` | `#121214` | `#ffffff` | node card body |
| `card_stroke` / `_hover` / `_selected` | `#343439` / `#5a5a61` / `#e6e6ea` | `#d4d4d9` / `#a0a0a8` / `#18181b` | card border by state |
| `text` / `text_muted` | `#e8e8ec` / `#8a8a93` | `#18181b` / `#6b6b74` | body text / labels and hints |
| `handle_fill` / `handle_glyph` | canvas / text | canvas / text | the `+` handle |
| `accent` | `#4cc38a` | `#1f9d63` | focus and selection in chrome |

Sizes (points at 100% zoom): grid spacing 20, dot radius 1 (fixed on screen), card radius 14,
panel radius 12, card padding 16, label gap 6, label 10.5 pt monospace, body 13 pt, handle radius 9.

Tests in `theme.rs` keep body text ≥ 7:1 against cards, labels ≥ 4.5:1 against the canvas, and the
selected border more visible than the resting one, in both themes.

## Canvas interaction (M0)

| Input | Effect |
|---|---|
| Drag empty canvas (primary) or drag anywhere (middle or right button) | pan |
| Two-finger scroll / mouse wheel | pan |
| Pinch, or ⌘/Ctrl + scroll | zoom about the pointer (10%–400%) |
| ⌘/Ctrl + `=` / `-` | zoom about the centre by 1.25× |
| ⌘/Ctrl + `0`, or click the zoom badge | 100%, centred on the cards |
| Drag a card | move it; click selects it, click on empty canvas clears the selection |

egui's own keyboard UI zoom is turned off so these keys zoom the canvas only.

## Level of detail and performance

- Grid dots keep a fixed screen size. When zooming out would put them closer than 12 points, the
  grid skips every other dot (`canvas::grid_step`), so it never turns into a haze; at most 120,000
  dots are drawn per frame, and they are tiny squares (4 vertices) rather than circles.
- Card text is laid out at whole-point sizes so continuous zooming doesn't rasterise a new glyph
  size every frame. Below 35% zoom a card shows only its label; text under 5 pt is not drawn.
- Cards outside the viewport are not painted.
