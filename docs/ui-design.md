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
| `accent` | `#4cc38a` | `#1f9d63` | focus and selection in chrome; box-select outline |
| `marquee_fill` | `#4cc38a` at 14% | `#1f9d63` at 11% | box-select rectangle fill |
| `duplicate_offset` | 40 world points | 40 world points | duplicate displacement on both axes, independent of zoom |
| `danger` | `#f06a5f` | `#c4372b` | errors and failed states in chrome |
| `wire_text` / `_image` / `_video` | `#d9b45a` / `#5aa7e0` / `#c678dd` | `#9a7414` / `#1f6fb0` / `#8a3fb0` | noodles and port rings by modality |
| `wire_audio` / `_mesh` / `_document` | `#e07a5a` / `#8fc35a` / `#a0a0a8` | `#b0482a` / `#4f8a1f` / `#6b6b74` | (a test keeps each ≥ 3:1 against canvas and cards, and all distinct) |

Sizes (points at 100% zoom): grid spacing 20, dot radius 1 (fixed on screen), card radius 14,
panel radius 12, card padding 16, label gap 6, label 10.5 pt monospace, body 13 pt, handle radius 9, input port radius 5. Noodles are 2 points wide (3.5 selected, over a `card_stroke_selected` halo), at any zoom. Cards are 320 points wide, at least 220 high, with 64 points per
additional schema row (720 maximum); control gaps are 6 points. Longer bodies scroll internally.

Tests in `theme.rs` keep body text ≥ 7:1 against cards, labels ≥ 4.5:1 against the canvas, and the
selected border more visible than the resting one, in both themes.

## Canvas interaction

| Input | Effect |
|---|---|
| Drag empty canvas (primary) or drag anywhere (middle or right button) | pan |
| Two-finger scroll / mouse wheel | pan on the canvas; scroll parameters over an expanded card |
| Pinch, or ⌘/Ctrl + scroll | zoom about the pointer (10%–400%) |
| ⌘/Ctrl + `=` / `-` | zoom about the centre by 1.25× |
| ⌘/Ctrl + `0`, or click the zoom badge | 100%, centred on the cards |
| ⌘/Ctrl+F outside unrelated editors, popups and gestures | open/focus canvas search |
| Enter / Shift+Enter in search | next / previous result, wrapping after the scan completes |
| Escape in search | close search and retain selection |
| Shift+1 / Shift+2 outside editors and gestures | fit all / selected nodes |
| Shift+M, or Hide map / Show map | toggle the minimap |
| Click or primary-drag the minimap | center the canvas on that world point |
| Drag a card background | move through `node.move`, one undo step per gesture; click selects it |
| Click a card / Shift-click | select only it / add or remove it from the selection |
| Shift-drag empty canvas | box select: cards the box touches join the selection (live while dragging); Escape cancels the box |
| Click empty canvas, or Escape outside text fields | clear the selection (Escape closes an open menu or cancels a box select first) |
| ⌘/Ctrl+A outside text fields | select every node on the canvas |
| Drag from a card's `+` output | draw a noodle; inputs that accept it get a ring. Drop on an input to connect (`edge.connect`); a single input that is already fed swaps its noodle in the same undo step (`edge.rewire`). One output fans out to any number of inputs |
| Drag from a connected input | pick up its (last) noodle: drop it on another input to reroute (`edge.rewire`), or on empty canvas to disconnect |
| Escape while dragging a noodle | drop it without changing anything |
| Click a noodle / Shift-click | select it / add or remove it; box select also takes noodles it crosses |
| Delete or Backspace outside text fields | delete the selected nodes and noodles in one undo step (`canvas.delete`) |
| `T` / `I` / `V` / `A` / `R` (no modifier, outside text fields) | quick-add a Text / Image / Video / Audio / Router node centred under the pointer and select it (`node.create`, one undo step; a quick-added router carries text, since `node.create` sets no config). Keys come from each kind's `quick_add`, so new kinds bring their own |
| Double-click empty canvas | the quick-add palette: type to search node kinds and the commands that can run now, ↑↓ to move, ↵ to choose, Escape or a click elsewhere to close. A kind is created where you double-clicked |
| ⌘/Ctrl+C, X, V outside text fields | copy / cut / paste the selected nodes and the noodles between them through the OS clipboard (versioned JSON, so another project or window can paste them); paste lands centred on the pointer with fresh ids and selects the result, one undo step (`canvas.paste`). Generation results are not copied. Plain text pastes as a Text node |
| ⌘/Ctrl+D outside text fields | duplicate the selection, offset 40 points |
| Right-click (or long-press) a card | menu: select the upstream / downstream chain of the selection, or all nodes of the card's kind; an unselected card becomes the selection first |
| Double-click the title | rename; Enter or blur commits, Escape cancels; empty restores the automatic title |
| Edit a parameter | schema-typed value through `node.param_set`; Enter or blur commits, ⌘/Ctrl+Enter for multiline text |
| ⌘/Ctrl+Z / ⌘/Ctrl+Shift+Z outside text fields | undo / redo document edits |

egui's own keyboard UI zoom is turned off so these keys zoom the canvas only. Canvas keyboard
shortcuts yield to focused text fields. Invalid drops (different modality, a list into a single input, a node into itself, a loop, or a
second copy of an existing connection)
change nothing and say why in the notice. Selection is shell view state: it never changes the document
or the undo history, and ids of removed nodes drop out of it. Grabbing or right-clicking a card
that is already selected keeps the rest of the selection; dragging still moves only the grabbed card
(moving a group is a separate change). Invalid parameter drafts stay visible until corrected or
cancelled; changing another field does not discard them. Slider drags preview locally and commit
once on release. Titles default to the first nonempty prompt/text line, or the kind label, and are
clipped to 64 Unicode characters. Unknown node kinds retain their title and show an unavailable
message. Asset controls display/remove existing references; file import is tracked separately (#63).

## Level of detail and performance

- **Frame budget (#65).** `cargo test -p nodecraft-ui-egui --release perf -- --ignored --nocapture`
  times frames over generated 200- and 1,000-node graphs (text sources and relays with a toggle,
  slider, choice and note, wired in rows, in a square grid) while panning at fixed zooms, sweeping
  zoom from 10% to 100% and back, and on the frame after an edit, with the pointer moving. It
  measures the egui pass (with AccessKit) plus tessellation at 1 and 2 pixels per point, not
  windowing, texture upload or GPU work. On an Apple M3 Pro, 2026-10-17: 200 nodes stay under
  2 ms mean (worst frame 6.8 ms, in the 2× zoom sweep); 1,000 nodes with 736 cards on screen pan
  at 4.3 ms mean, the frame after an edit costs 5.4 ms and the worst sweep frame 10.6 ms, all
  inside 16.7 ms for 60 fps. The run found one hotspot, fixed: every edit made the schema cache
  rescan all entries per visible card (O(visible²), 10.2 ms after an edit at 1,000 nodes); it
  now keeps a running byte total and a shared-descriptor index. Title font sizes still change
  with every zoom step (a new glyph size per frame in a sweep); snapping them to a few sizes is
  the next lever if sweeps get heavier.
- Grid dots keep a fixed screen size. When zooming out would put them closer than 12 points, the
  grid skips every other dot (`canvas::grid_step`), so it never turns into a haze; at most 120,000
  dots are drawn per frame, and they are tiny squares (4 vertices) rather than circles.
- Card text is laid out at whole-point sizes so continuous zooming doesn't rasterise a new glyph
  size every frame. Below 55% zoom a card shows only its label. Labels stay at least 9 screen points even at 15%; body controls appear only at readable scale.
- Cards outside the viewport are not painted.

## Review corrections for graph cards

Unfocused text fields show a preview of at most 256 characters. The complete graph Value is
retained, and the full editor/draft is created only when the user focuses a field. Cancelled or
pristine blurred editors return to previews. Validated instance descriptors and visibility are
cached lazily for visible nodes per session revision; idle frames borrow supplied/default values.
Static descriptors share one Arc, and dynamic descriptors plus visibility entries share a 16 MiB
aggregate view-cache budget with least-recently-used eviction. Culled nodes allocate no schema cache. Four near-1MiB prompts
have a warm-idle regression and manual release benchmark. A separate exact-1MiB case proves that
focus/cancel never truncates the unseen tail. Card motion previews in view state and dispatches
one node.move on release, avoiding full-document work for each pointer sample.

Cards have accessible names and focusable Rename actions; F2 or Enter on a focused card/title
opens its title editor. Titles use the first nonblank line, trim whitespace and display an
ellipsis. A whitespace-only explicit title restores the automatic title. Overview labels use a
9-point minimum screen size and bodies are hidden below 55% zoom. AccessKit control rectangles are
tested at 2x; LOD/culling tests assert that hidden controls expose no input nodes.

Sliders defer typed changes until Enter/blur and commit pointer drafts on release. Optional
numeric bounds use text editors instead of invented slider ranges; signed/unsigned integers and
seeds stay exact decimal values and obey schema steps/maxima. Parameter descriptions become
label tooltips. Validation uses byte limits; over-limit multibyte drafts show an explicit byte
limit error and remain editable rather than truncating persisted content.

Undo/redo uses command shortcut metadata (including Ctrl+Y), and does not erase invalid or dirty
drafts. Escape belongs to the active editor even when egui drops focus first. A dirty field or
title hidden by zoom/culling remains a draft, with a visible unsaved notice and explicit discard
action. Hidden conditional fields keep their draft and suppress unresolvable field errors; the
same notice offers a way to discard. These drafts are not silently committed during rendering.

New sizing tokens cover preview length, editor rows, title minimum size and handle hit/stroke
sizes. The selected card is painted last. The home bounds are computed only for startup/reset.

## Minimap and fit navigation

The bottom-right minimap shows actual instance card bodies, highlights selected nodes, and outlines
the current viewport. Its toolbar keeps Fit all, Fit selected, the map toggle and the existing zoom
reset discoverable. Map visibility, pan and zoom are session-local view state, never project data or
undo entries. Default map size is 320×160 points, with a 30-point toolbar, 16-point outer margin
and 6-point gaps; these dimensions and the 24-point fit padding are theme tokens.

Fit resolves actual instance geometry for every node, including offscreen nodes, and includes titles,
unsaved labels and handles. Every instance hook and node payload is checked; identical borrowed descriptors share successful
validation within a refresh, while owned dynamic descriptors validate individually. Startup/reset
uses the same geometry path. Only lightweight rectangles survive between document revisions.
Unknown kinds and failed instance/schema admission use the complete painted placeholder, which
remains navigable. Invalid payloads or unavailable/nonfinite geometry disable Fit all; Fit selected
requires valid geometry for every selected node. This navigation fallback does not provide
authoritative layout geometry. Empty sets and invalid/tiny viewports cannot fit.
Fit reserves space above the overlay and clamps zoom to 10%–400%. When widely spread nodes cannot
all fit at 10%, it centers them at minimum zoom and reports that limitation.

Minimap gestures own the pointer through release even outside the map, with a frozen transform
so the target cannot chase the changing viewport. Map input cannot start a canvas gesture beneath
it, interrupt a card/box/pan gesture, or act through a popup or active text editor.

The new `canvas.fit_all`, `canvas.fit_selection`, `canvas.minimap_toggle` and `canvas.pan_to`
commands use the engine service path. Live hosts use `NodeCraftApp::dispatch_navigation(ctx, id,
params)` (and `navigation_enabled`) to compute current geometry, selection and focus, temporarily
install the capability, dispatch, restore prior resources and apply the intent immediately. A raw
headless Session has no live capability and returns Unsupported. Existing services/resources are
preserved. The older 100% reset button and shortcut retain their existing view-only path.

## Canvas search

Search opens from the top-right Search button or ⌘/Ctrl+F. It matches complete stored node titles,
Text values throughout parameter/configuration lists, and the linked tag title. Automatic prompt
labels match through their full stored prompt/text, including tails beyond the card preview.
When the card uses a kind-label fallback, search uses the same checked instance label. Unavailable
kinds still match their stored title/text; their automatic fallback is “Unknown node.” Model
matching currently means persisted textual identifiers/configuration. Catalog display labels and
unstored schema defaults are not indexed; catalog integration can add verified labels later.

Matching is literal and case insensitive through Rust Unicode scalar lowercase expansion, including
İ→i plus combining dot and final sigma ς→σ. It is not regex, locale-dependent matching, Unicode normalization or full
case folding (ß does not become ss). Each field is independent. Leading/trailing Unicode whitespace
in the query is ignored; an empty query returns no matches and leaves selection alone. Queries
above 1024 UTF-8 bytes are rejected with an error; a bounded editor draft permits displaying and
correcting that error instead of accepting a silently truncated query.

Scanning borrows text, keeps a UTF-8 cursor plus small KMP state, and charges at most 512 KiB of source
text and 16384 structural/path work units per frame. Empty/nested values count too. A chunk boundary
never loses a partial match. Results retain at most one NodeId per actual validated canvas node,
ordered by NodeId, without a truncated result list or full-prompt lowercase copies. Revisions,
canvas changes and queries restart the scan; unchanged idle frames do not scan. The existing
geometry refresh also retains one fallback label per node (at most 256 bytes each, at most 2.56 MB
of label bytes under today's graph node cap, plus map overhead); descriptor validation precedes
copying, borrowed validation is shared and owned descriptors are not retained. Queries do no
instance resolution. Startup/reset shares this cache.

The overlay shows progress, match count, Previous, Next and Close search. Its status is a polite
AccessKit live region; unchanged frames retain unchanged accessibility data. Typing is accepted before
Enter in the same frame. While incomplete, one generation-tagged next/previous request waits for
the complete list; another request replaces it, so wrapping cannot use partial or old results.
Query changes select/frame the first result after completion; document edits keep the current ID
when it still matches and clear it otherwise, without moving the camera or replacing selection.
A document revision cancels pending navigation from the interrupted scan. Next/previous wrap deterministically. Closing cancels deferred framing and
retains the query and document selection. No matches leave the previous selection unchanged.

Framing preserves zoom when the complete card fits, panning only as needed; otherwise it zooms out.
It uses actual instance card/title/handle bounds, reserves both overlays, selects the result
in view state and reports minimum-zoom or unavailable-geometry limitations. The search input alone
has a focus exemption for framing. Opening yields to unrelated node editors so ⌘/Ctrl+F cannot
blur-commit a dirty title/parameter. Popups and existing gestures retain ownership. Search Escape
is consumed before canvas selection Escape, including egui's early focus-loss transition.

`canvas.search_open`, `canvas.search_query`, `canvas.search_next`, `canvas.search_previous` and
`canvas.search_close` share the engine service path. Live callers use
`NodeCraftApp::dispatch_search(ctx,id,params)` / `search_enabled`; state changes apply immediately,
scans progress on rendered frames, and queued framing waits for completion and input availability.
Scoped capabilities restore prior resources on errors/success and preserve unrelated services.
Raw headless Session calls are Unsupported. Search creates no document revisions, undo entries or
events. Search width 420, expanded height 128 and collapsed width 130 points are theme tokens.

## Clipboard admission and placement

Copy projects only retained node fields and inner edges; generation history and active results
never enter the clipboard projection. The borrowed projection is checked before constructing
owned nodes: the clipboard envelope is at most 16 MiB encoded JSON, and the eventual command is
at most 1 MiB of logical key/string bytes and 16,384 JSON values under the shared depth rules.
Selections beyond those limits are refused with an instruction to copy fewer nodes.

Cut first checks the actual registered paste against a privately staged post-delete document.
The delete commits through ordinary graph/history/recovery admission only after that roundtrip
passes, then the app queues the copied text. This validates the captured post-delete proposal;
a subsequent Paste remaps IDs and placement against its current document and revision. It does
not guarantee acceptance by arbitrary extensions for every future paste proposal. A failed append
can require recovery repair while preserving the live document and history. This cannot acknowledge
delivery by the OS clipboard; undo remains available after a successful Cut. Command/kind extensions keep the existing trusted
pure-description contract; this is not a sandbox for arbitrary native callback side effects.

Media references require actual immutable destination bytes even when project UUIDs match.
Already-present bytes may be used across different UUIDs; the clipboard transfers no assets.
The existing format checks known MIME signatures and declared image width/height. Unknown
signatures remain availability-only; duration/audio/page metadata remains advisory.

Busy editors, Search, palettes, menus, the keys panel and active canvas/minimap gestures yield
before copying Paste text. For an idle canvas, only the final Paste event in one platform input
is copied; an oversized final event cannot fall back to an earlier paste. Fresh minimap primary
press ownership uses the first press position and the same frozen admission as later capture,
including movement outside the map within that input. Ignored events remain unconsumed.

Pastes over the minimap or its toolbar use the unobscured canvas center; ordinary canvas pointers
retain their target. No finite unobscured region means refusal. Both node groups and plain Text
use actual admitted instance body geometry, including renderer placeholder refusal. Duplicate
uses the `duplicate_offset` theme token in world coordinates.
