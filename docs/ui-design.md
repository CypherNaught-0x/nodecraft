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
| `mask_overlay` | `#4cc38a` at 60% | `#1f9d63` at 60% | the painted area in the mask editor, scaled by mask strength |
| `duplicate_offset` | 40 world points | 40 world points | duplicate displacement on both axes, independent of zoom |
| `danger` | `#f06a5f` | `#c4372b` | errors and failed states in chrome |
| `stale` | `#e0a83a` | `#8a5800` | the badge on a card whose result no longer matches its inputs; its text is `card_fill` |
| `prompt_fill` | `#1a1a1d` | `#f4f4f5` | a generation card's prompt composer (text ≥ 7:1, labels ≥ 4.5:1 against it) |
| `mention_fill` | `#23362d` | `#d5eee1` | behind an `@` mention chip in a prompt being edited (text ≥ 7:1 against it) |
| `wire_text` / `_image` / `_video` | `#d9b45a` / `#5aa7e0` / `#c678dd` | `#9a7414` / `#1f6fb0` / `#8a3fb0` | noodles and port rings by modality |
| `wire_audio` / `_mesh` / `_document` / `_style` | `#e07a5a` / `#8fc35a` / `#a0a0a8` / `#e06fa8` | `#b0482a` / `#4f8a1f` / `#6b6b74` / `#a82f6c` | (a test keeps each ≥ 3:1 against canvas and cards, and all distinct) |
| `waveform` | `#5c5c66` | `#a8a8b2` | a sound card's waveform still to play (≥ 2:1 against the canvas); what has played is `wire_audio` |

Sizes (points at 100% zoom): grid spacing 20, dot radius 1 (fixed on screen), card radius 14,
panel radius 12, card padding 16, label gap 6, label 10.5 pt monospace, body 13 pt, handle radius 9, input port radius 5. Noodles are 2 points wide (3.5 selected, over a `card_stroke_selected` halo), at any zoom. Cards are 320 points wide, at least 220 high, with 64 points per
additional schema row (720 maximum); control gaps are 6 points. Longer bodies scroll internally. The composer's submit button is 28
points across; the generation toolbar is 32 high, 10 above its card; the image viewer opens 720 wide. A sheet's table has 32-wide checkbox and number columns and
120-wide model, references, result, status and button columns, the prompt taking the rest; the
Sheets panel lists canvases 320 high, with a 250-wide title field. The Layer Editor's layers and
properties panels are 250 wide; a dragged layer snaps within 6 points of a guide, and its rotate
handle sits 24 points above it. App mode's form is 640 wide, and its media and result
thumbnails 120 across. The Timeline editor shares the Layer Editor's panel width; its clip track
is 84 high under a 22-high time ruler, and a clip's edge trims within 6 points.
The ComfyUI servers panel's jobs-at-once text field is `comfy_jobs_width` (48) wide,
and its new-server name field is `comfy_name_width` (160) wide.
A card's image preview, and a text card's answer, take `card_preview_height` (180) above the
composer; a long answer scrolls inside it and can be selected and copied. While a wire feeds a
card's prompt the prompt is shown disabled, its label says it comes from connected text (on the
label's own row, so the card keeps its size), and its hover text explains that nonempty connected
text is used instead; the typed prompt is kept and comes back when the wire is removed. A
generation kind other than images is offered only when the catalog has a model for it.

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
| Delete or Backspace outside text fields | delete the selected nodes, noodles and field in one undo step (`canvas.delete`; a deleted field's cards stay) |
| `T` / `I` / `V` / `A` / `R` (no modifier, outside text fields) | quick-add a Text / Image / Video / Audio / Router node centred under the pointer and select it (`node.create`, one undo step; a quick-added router carries text, since `node.create` sets no config). Keys come from each kind's `quick_add`, so new kinds bring their own |
| Double-click empty canvas | the quick-add palette: type to search node kinds and the commands that can run now, ↑↓ to move, ↵ to choose, Escape or a click elsewhere to close. A kind is created where you double-clicked |
| ⌘/Ctrl+C, X, V outside text fields | copy / cut / paste the selected nodes and the noodles between them through the OS clipboard (versioned JSON, so another project or window can paste them); paste lands centred on the pointer with fresh ids and selects the result, one undo step (`canvas.paste`). Generation results are not copied. Plain text pastes as a Text node; a copied image (desktop) pastes as an Image node at the pointer, read off the UI thread, encoded as PNG and held to 16384×16384 pixels and 16 MiB, one undo step (#319) |
| ⌘/Ctrl+D outside text fields | duplicate the selection, offset 40 points |
| `F` (no modifier, outside text fields) | make a field: around the selected cards, which become its members, or an empty 320×240 zone at the pointer (`field.create`, one undo step). The zone takes in every selected card, including one added this frame. The new field is selected |
| Drag a field's header / its lower-right corner | move the field and every member (`field.move`) / resize the zone alone (`field.update`); one undo step on release, Escape cancels. Double-click the header to collapse or expand it. Shift-drag from a header or corner draws a box select, as anywhere else on the canvas |
| Right-click a field's header | rename, collapse or expand, fit to members, colour, or remove the field (its nodes stay) |
| Right-click a card → **Field** | move the selected nodes into a field, or take them out (`node.field_set`) |
| Right-click (or long-press) a card | menu: select the upstream / downstream chain of the selection, or all nodes of the card's kind; an unselected card becomes the selection first |
| Double-click the title | rename; Enter or blur commits, Escape cancels; empty restores the automatic title |
| Enter on a focused generation card, ⌘/Ctrl+Enter in its prompt, or its round arrow button | quote a run of that card (`run.quote`) and show its price, anything blocking it and any unknown part (a by-usage step whose provider can quote it is asked first, `run.estimate`, and the price shown again with its answer); **Generate for …** starts priced work (`run.start` with the quote's token and totals). The confirmation counts cached reuse as **N reused, free**; with only cached results, it says results are used where needed, already-active results stay in place, and nothing generates. Its button is **Continue without generating**: `run.start` applies any needed cached results but does nothing when they are already active. Cancel or Escape starts nothing. An edit after the quote makes it stale: nothing starts until it is quoted again. If the start fails, the dialog offers only Close, since the host may have taken the run. F2 or double-click still renames a generation card |
| Click a card's **Stale ↻** badge (shown when its result no longer matches its inputs) | quote a refresh of that card with whatever upstream is stale or missing (`run.quote`, mode `with_upstream`) and confirm it as above, in a dialog titled **Refresh** |
| Click **Queue** (top-right, beside **API keys**; **Queue (N)** while N jobs are queued or running) | open the queue panel under search: every running and recent job with its card, provider and model, status, and price. **Cancel** stops a live job; **Retry** quotes a failed or cancelled card again like Generate; **Clear finished** forgets ended jobs. A cancelled job never sent shows **Not charged**; one that may have reached the provider shows its estimate as **unresolved**. Cancel asks the provider to stop; if it refuses or doesn't answer, the job shows **Cancel refused** and is followed to its end, since it may still be billed. A paid result that couldn't be kept on its card (it didn't fit its ports, couldn't be converted or couldn't be recorded) is **held** (#369): **Queue (N, M held)** counts them and the panel lists each with why, and **Retry**, **Put on canvas** and **Discard**; none calls the provider again. While 16 are held, new jobs are refused unsent. The desktop app saves held results so they survive a restart (#614). Cards show their own status under them (**Queued**, **Generating 40%**, **Run failed: API key**, with the full error and a hint on hover) |
| Click **Plugins** (top-right, beside **API keys**) | open the plugins panel (#172); in the browser it says packages last only for the session and can't run there (#823): every installed package with its version, state, licence, signer and grant. Type or **Choose…** a `.ncraft-plugin` file and **Check** it: a dry run shows who signed it (or that nobody did), its licence and the text of its licence and notice files, the IDs it adds and the capabilities and addresses it asks for (and, for an update, what is new), with nothing installed; **Install** or **Update** then imports it disabled. **Grant…** shows what a grant would allow, required capabilities ticked and fixed, optional ones to tick, before **Grant**; a data package says it needs none. **Enable**, **Disable**, **Revoke**, **Roll back to …** (a kept version, while disabled), **Uninstall** (asks to confirm) and **Remove unused versions** each dispatch one `plugins.*` command |
| `−` / `+` beside a generation card's prompt | step the model's image count (the parameter tagged `generated_image_count`) within its range |
| Select one generation card | a toolbar above it: the model (opens the model picker), the aspect ratio (the choice tagged `aspect_ratio`, or else one whose options are all `W:H`), and Expand once it has an image |
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
message. Asset controls display/remove existing references; OS file drop and desktop clipboard image import create source nodes (#197, #63).

Fetched option pickers open at the current choice. Their filter matches labels and stored
values without editing the document, and shows progress while scanning a large list. Popup
width stays fixed and long rows truncate. Use default clears the stored override through the
same undoable parameter edit, including when the old value is no longer listed. Unknown lists
keep the editable text control and Refresh guidance. Reset is disabled without a stored override.

## Level of detail and performance

- **Frame budget (#65).** `cargo test -p nodecraft-ui-egui --release perf -- --ignored --nocapture`
  times frames over generated 200- and 1,000-node graphs (text sources and relays with a toggle,
  slider, choice and note, wired in rows, in a square grid) while panning at fixed zooms, sweeping
  zoom from 10% to 100% and back, and on the frame after an edit — a move, and (#556) a parameter
  change on a node that is not an action — with the pointer moving. It
  measures the egui pass (with AccessKit) plus tessellation at 1 and 2 pixels per point, not
  windowing, texture upload or GPU work. Selection, palette and clipboard interactions are not
  benchmark workloads. Historical observations on an Apple M3 Pro, 2026-10-17: 200 nodes stayed under
  2 ms mean (worst frame 6.8 ms, in the 2× zoom sweep); 1,000 nodes with 736 cards on screen panned
  at 4.3 ms mean and the worst sweep frame was 10.6 ms. These are historical CPU-side samples,
  not current frame-time or native 60 fps guarantees; rerun the command for current edit timings.
  The run found one hotspot, fixed: every edit made the schema cache
  rescan all entries per visible card (O(visible²), 10.2 ms after an edit at 1,000 nodes); it
  now keeps a running byte total and a shared-descriptor index. Live recompute (#88) later made
  every edit plan each action node alone, rebuilding the planner graph per node (O(nodes²), 284 ms
  after an edit at 1,000 nodes); one walk now decides for all of them, the project is copied only
  when a cached result applies, and a settled canvas skips the look entirely for moves and
  grouping (#549). In PR #551's pre-merge measurement (landed as `9867f144`), Apple M3 Pro,
  2026-10-21, load average about 18: 2.3 ms after `node.move` at 200 nodes and 7.8 ms at
  1,000, against 1.7 and 5.4 ms panning. These are CPU egui preparation plus tessellation,
  **not** presented frames; the move takes the settled-layout fast path and this busy-machine
  result is not a quiet remeasurement of full parameter-edit planning. The independent desktop
  presented-frame measurement used to close #65 is in [performance.md](performance.md#the-60-fps-criterion).
  Title font sizes still change with every zoom step (a new glyph size per frame in a sweep);
  snapping them to a few sizes is the next lever if sweeps get heavier.
- **The frame after a parameter edit (#556).** A parameter edit changes what an action reads, so
  nothing skips it, and #556 adds it to that command: `node.param_set` on the text source at the
  head of a row of the same generated graphs, at the same 10% zoom and in the same span (the egui
  pass with AccessKit plus tessellation; the edit itself is untimed, dispatched just before the
  frame it is measured in), with a move loop from the same process printed beside it. These are
  load-condition numbers, not quiet-machine measurements: five runs on an Apple M3 Pro, 12 CPUs,
  2026-10-21, at load average 12-15. The least-contended of them, at 1 pixel per point: 2.27 ms after a move and 4.95 ms after a parameter edit at 200 nodes, 7.97 and
  22.93 ms at 1,000; the median of the five is the same 2.27 against 5.14, and 9.31 against 27.03
  ms. At 2 pixels that same run reads 2.18 against 4.97 and 8.02 against 22.30 ms. The moves took
  no live look at all and the edits one per frame, which is the difference the pair is measuring:
  the edit costs about 2× a move at 200 nodes and about 3× at 1,000. The gap grows with node count
  because the edit runs the O(nodes) `live_actions::pending` path that a move skips. The move half
  also lands on the #549 line above, 2.3 and 7.8 ms, within 0.2 ms, which is what makes the two
  worth reading together. #203's frame after an edit was 1.82 and 6.21 ms; these loaded runs
  cannot establish a comparable change from that quiet-machine baseline. A quiet re-measure is
  tracked in #743. The spread matters as much as the figures: runs taken earlier the same day, while other builds held the load average at 40-55, came out up to 3× worse
  (a 1,000-node pan read 9.4 ms in one and 27.7 ms in the other). Pass `--test-threads=1` when
  rerunning, or the ignored cases share the machine with each other on top of everything else.
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

## Discussion markers

Discussion bubbles stay 30 screen points at every zoom, with a 6-point tail, 3-point active ring
inset, 6-point corner radius and 12-point gap from node decorations. Open bubbles use primary text
as fill with canvas-colour count text; resolved bubbles use panel fill, a primary-text outline and
checkmark. The active bubble adds a second outline. The local Show resolved checkbox explicitly
uses the primary-text stroke for both its unchecked boundary and checked glyph in both themes.
Initial node offsets include the bubble's ring and gap outside the actual card bounds. A selected
marker behind the Comments panel moves beside it with a leader; its stored anchor stays fixed.

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
Fit all frames what the map draws: a collapsed field counts as its header, not its hidden
cards (#739), and with only collapsed fields on the canvas it fits their headers.
Unknown kinds and failed instance/schema admission use the complete painted placeholder, which
remains navigable. Invalid payloads or unavailable/nonfinite geometry disable Fit all; Fit selected
requires valid geometry for every selected node. This navigation fallback does not provide
authoritative layout geometry. Empty sets and invalid/tiny viewports cannot fit.
Fit reserves space above the overlay and clamps zoom to 10%–400%. When widely spread nodes cannot
all fit at 10%, it centers them at minimum zoom and reports that limitation.

Minimap gestures own the pointer through release even outside the map, with a frozen transform
(so the target cannot chase the changing viewport), and suppress Delete, Backspace, Undo and Redo
until the release frame has finished. A fresh press still admits ordinary undo/deletion before
deciding minimap ownership. Clipboard copy/cut/paste/duplicate and quick-add
yield from the initial map press through release, including keys in the same input frame and a
pointer outside the map. Rejected shortcuts remain unconsumed. The initial primary-press position
decides ownership even when later motion crosses the map boundary in that frame. If a frame batches
several primary presses, its first press decides; a later visibility shortcut cannot retrospectively
acquire that press. Map input cannot start a canvas gesture beneath it, interrupt a card, box, pan
or discussion-pin gesture, or act through a popup or active text editor. A cancelled discussion-pin
gesture keeps ownership until release. Admission refused before a same-frame cancellation stays
refused until a fresh press.

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
events, with one exception: stepping to a card inside a collapsed field expands that field first, as
one `field.update` undo step (which, like any edit, clears redo). If the field can't be expanded,
its header is selected and framed and a notice says the match is inside. Search width 420, expanded height 128 and collapsed width 130 points are theme tokens.

### Format selection

The selection toolbar and selected-card context menu offer **Grid** and **Dependency order**.
Both dispatch `canvas.auto_format` and move selected nodes in one undo step; unselected nodes
and edges stay fixed. Fit selected remains a separate view command. No layout shortcut is
reserved. Finish an editor or gesture before opening Format; dirty hidden drafts remain drafts.

Actual resolved instance sizes include the clipped title/UNSAVED line and output handle stroke
at reference zoom 1.0. Unavailable instances retain navigation placeholders, but lack the checked
logical sizes required for layout; formatting rejects the whole selection atomically.
Persistent positions are independent of camera zoom. Auto-format uses `layout_gap` (64 world points) and `layout_button_width` (150
screen points). Outer cell origins and spans deliberately align to an **8-world-point lattice**;
this is the layout policy for stable repeated formatting, not coordinate clamping. Positive
left/top insets are added to each cell origin to position the body. Body positions outside the
canvas coordinate domain reject the entire action. Grid defaults to ceil(sqrt(selection count));
programmatic callers can supply columns. Dependency order ranks the whole DAG, including paths
through unselected nodes, then places selected ranks in left-to-right columns. It does not move
unselected obstacles or promise optimal edge crossings. Reapplication with unchanged selection,
logical extents and topology is a no-op, including after camera zoom changes and at large legal coordinates.
Grid ordering groups connected positive vertical-overlap intervals, then orders each group by
left edge and stable node ID. A tall card can bridge an overlap chain; touching boundaries start
a separate row. The output lattice does not define input row membership.
Enabled checks perform bounded structural admission without arranging, ranking the DAG or cloning
the project. A final output-coordinate or transaction error can still reject dispatch atomically.
Unavailable toolbar primary clicks are consumed by a one-pass, exact-focus and topmost-layer
capability before widgets render; keyboard, scroll, other buttons and popup owners remain active.

### Colour tags

Colour tags use `Tokens::tag_colors` (eight named palette entries), `tag_marker_width`, `tag_panel_width`, `tag_panel_height`, `tag_row_height`, `tag_definition_row_height` and `tag_identity_size`. Dark and light colors are deliberately distinct; unknown/missing palette identifiers use `text_muted`. A marker identifies the tag independently of the selected border. Filter matches use the existing accent token plus dashed inset corners, while nonmatches use the quiet panel stroke without fading text. Selected-card borders remain intact. Tag lists use themed two-line entries, with the exact stable ID above the bounded title; definition controls are on the row’s context menu. Behavior, command inventory and persistence compatibility are specified in [color-tags.md](color-tags.md).

### Presence

Other people in a shared project (#120) are drawn in `Tokens::presence_colors` (eight per
palette, picked by their place in the room), above the cards and clipped to the canvas: a pointer
`presence_pointer` tall, outlined `presence_outline` in `presence_ink`, with a pill holding their name in `presence_ink`, and a `presence_ring`
outline round each card they have selected. Their initials sit beside the Share chip, each a
`presence_avatar` circle; hovering one says who and what they may do, and clicking it opens their
canvas centred on their pointer. Nobody is drawn while the connection is being made again, and
nothing is said about where you are while a private canvas is open.

### Fields

Fields (#66) draw on the canvas layer, under the noodles and cards, in their palette colour
(`Tokens::tag_color`) at `field_fill_opacity` (0.10, doubled for the header band). The header is
`field_header_height` (32) tall, or taller at low zoom so its title stays readable; the selected
field's outline uses `accent`. `F` pads the selection by `field_padding` (32) plus the header;
a resize stops at `field_min_width` × `field_min_height` (160 × 96). Membership is explicit:
moving a card over a zone doesn't join it. A collapsed field shows only its header: members
aren't drawn, select-all and box select skip them, and their noodles end at the header. Search
and tag navigation that land on a hidden member expand its field first (one `field.update` undo
step), then select and frame the card. Member counts and the hidden set are worked out once per
document revision, not per frame. The minimap likewise draws the header (in `panel_stroke`) and
not the members.

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

## PDF reader

The PDF reader (#590) is a window over one PDF: thumbnails `reader_thumb_width` (112 screen points)
wide on the left, the page on the right, zoomed from 50% to 400%. Pages have a 1-point `card_stroke` edge so a white
page shows on a light window; the current thumbnail is outlined in the accent colour. Dragging over the page selects a rectangle drawn with `marquee_fill` and
`marquee_stroke`, the canvas's box-select tokens; "New Text node from selection" reads the text inside it.

## Agent sidebar

The agent sidebar (#114, Cmd+/) is a right panel `agent_width` (360 screen points) wide; the canvas
takes the rest of the window. Its header shows the mode (Assist/Auto) and Clear, then a muted line
with the model and what auto mode has accepted of its budget; notices use `danger`. Steps waiting
for approval show what they would do (read, change the project, run and its price) with Approve
and Decline; Esc or Cancel stops the agent while it works.
