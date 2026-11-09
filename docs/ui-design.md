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
| `text` / `text_muted` | `#e8e8ec` / `#96969f` | `#18181b` / `#5f5f68` | body text / labels and hints (`text_muted` ≥ 4.5:1 on panels, cards and a hovered widget's `panel_stroke`) |
| `handle_fill` / `handle_glyph` | canvas / text | canvas / text | the `+` handle |
| `accent` | `#4cc38a` | `#1f9d63` | focus and selection in chrome; box-select outline. Strokes and fills only (3:1 is enough); never text |
| `accent_text` | `#4cc38a` | `#0f6b42` | the accent drawn as text: run status, **Running**, **Soul ID**, a verified key, links (≥ 4.5:1 on canvas, panel and card) |
| `selection_fill` / `selection_text` | `#1d3a2c` / `#4cc38a` | `#d5eee1` / `#0f6b42` | a selected row or entry: `selectable_label`, the palette's highlighted row, the model picker, the top bar's open panel, and selected text (≥ 4.5:1) |
| `marquee_fill` | `#4cc38a` at 14% | `#1f9d63` at 11% | box-select rectangle fill |
| `mask_overlay` | `#4cc38a` at 60% | `#1f9d63` at 60% | the painted area in the mask editor, scaled by mask strength |
| `duplicate_offset` | 40 world points | 40 world points | duplicate displacement on both axes, independent of zoom |
| `danger` | `#f06a5f` | `#c4372b` | errors and failed states in chrome; the **Can't check** badge (its text is `card_fill`) |
| `stale` | `#e0a83a` | `#8a5800` | the badge on a card whose result no longer matches its inputs; its text is `card_fill` |
| `prompt_fill` | `#1a1a1d` | `#f4f4f5` | a generation card's prompt composer (text ≥ 7:1, labels ≥ 4.5:1 against it) |
| `mention_fill` | `#23362d` | `#d5eee1` | behind an `@` mention chip in a prompt being edited (text ≥ 7:1 against it) |
| `wire_text` / `_image` / `_video` | `#d9b45a` / `#5aa7e0` / `#c678dd` | `#9a7414` / `#1f6fb0` / `#8a3fb0` | noodles and port rings by modality |
| `wire_audio` / `_mesh` / `_document` / `_style` | `#e07a5a` / `#8fc35a` / `#a0a0a8` / `#e06fa8` | `#b0482a` / `#4f8a1f` / `#6b6b74` / `#a82f6c` | (a test keeps each ≥ 3:1 against canvas and cards, and all distinct) |
| `waveform` | `#5c5c66` | `#a8a8b2` | a sound card's waveform still to play (≥ 2:1 against the canvas); what has played is `wire_audio` |

No egui default colour is left un-themed: `visuals()` sets egui's selection from `selection_fill` /
`selection_text`, its hyperlink colour from `accent_text`, and its warning and error text from `stale` and
`danger`, and the text cursor from `text`. `theme.rs` tests hold, in both themes, selection text on
`selection_fill`, `accent_text` on canvas, panel and card, `text_muted` on panels, cards and
`panel_stroke`, and warning and error text on panels and the canvas at 4.5:1 or more.

Stroke widths, a few sizes and egui's small text are tokens too (#1124): `stroke_hairline` (1),
`stroke_medium` (1.5) and `stroke_strong` (2) for outlines, playheads and chosen thumbnails or
clips; `heading_size` (19.5) for the drop hint and `display_size` (24) for an empty deck slide's
message; `inset` (2) between a thumbnail and its outline and for a swatch's corners; `dim` (0.6) for
a fill drawn dimmed behind what is being edited; and `badge_text_ratio` (1.2) for the number in an
input-order badge. `notice_edge` (3) and `notice_width` (380) size the notice stack (#1110). `theme::apply` sets egui's `TextStyle::Small` to `label_size` (10.5), so
`.small()` hints are never below a card label. `theme::panel_frame(t)` is the one framed-group
recipe (`panel_fill`, hairline `panel_stroke`, `panel_radius`, `card_padding`). A `theme.rs` test
fails on any literal `Stroke::new(<number>, …)` outside the theme and tests.

Control states (#1112):

| State | egui widgets | Painted controls (badges, submit arrow, thumbnails, noodles) |
|---|---|---|
| Rest | `control_fill`, a 1-point `control_stroke` outline (≥ 3:1 against `panel_fill`); hovered and pressed drop the outline so selectable rows keep a clean edge | their own colour |
| Hovered | `control_fill_hover` | `theme::State::Hovered.tint`: lifted toward the text colour; thumbnails take `card_stroke_hover`; a noodle is drawn `wire_width_hover` (3) wide and brighter, with a pointing cursor, within its hit band |
| Pressed | `control_fill_pressed` | lifted further, including a held noodle; the submit arrow's accent moves toward the text colour |
| Open (menu) | `control_fill_hover` | — |

Quiet buttons (the top bar, menus, selectable rows) stay frameless at rest by asking for it
(`frame_when_inactive(false)`, egui's menu style) rather than being the default.

Keyboard focus on painted canvas widgets (cards and Note cards and their titles, field headers,
stale and mask badges, the minimap) shows a ring (#1108): `accent`, `focus_ring_width` (2) wide,
`focus_ring_gap` (1) outside the widget (inside the minimap, whose edge clips), in screen points at
any zoom, clear of the selected border inside and a point short of the presence ring further out.
It shows when focus arrived any way but a pointer press (Tab, the arrow keys, a screen reader),
stays when Enter or Space activates the widget, and goes when a click elsewhere takes focus away. `focus::ring_if_focused`
is the one call a painted widget makes; a `theme.rs` test holds the ring at 3:1 against the canvas
and card fill in both themes.

Pointer targets are at least 24 × 24 screen points (#1114, WCAG 2.5.8): a noodle's hit band is
`noodle_hit` (12) either side of its line in screen points, whatever its painted width, and a card
under the pointer wins over a noodle; stale, mask and "Can't check" badges and presence avatars hit
in a `min_target` (24) square around their painted shape (`theme::at_least`; a badge grows down
and sideways only, never into the card above it, `theme::badge_hit`), at any zoom, painted as before. Ports already reach 26 points. In-card controls (19 world points tall) still shrink with
the canvas: refusing clicks below a zoom would break editing at overview zoom, so that part of
#1114 is left open.

The remaining scope is the controls inside a card: toggles and choices, slider tracks and their
exact-value entries, text previews/editors, model and asset actions, and the media controls. They
share the transformed card layer (`card.rs`); their row reservations come from `card/layout.rs`
and must not depend on the camera. Increasing `interact_size.y` alone cannot enlarge explicitly
sized text/slider rows, and expanding adjacent targets separately can make the slider and its
entry compete or cross the parameter viewport and ports. Disabling the entire body below a zoom
would also remove the scrolling that `scroll413_a_body_near_its_limit_takes_the_rest_of_its_room_and_the_canvas_the_remainder`
checks at 60 %. The existing slider/entry and text-edit tests explicitly retain editing at 75 %.
A complete follow-up therefore needs either disjoint screen-space hit cells for each generated
control (clipped to the body, with ports retaining priority), or a deliberate pointer-only
"Zoom in to edit" mode that preserves scrolling, existing drafts, keyboard editing and gestures
already in progress. Neither behavior is established by the badge/noodle fixes.

A disabled control keeps its explanation (#1113): egui hides `on_hover_text` once a widget is
disabled, so every `add_enabled(..)` that sets hover text also sets `on_disabled_hover_text`, written
as what to do ("Select at least one row to run"), and `ui_ext::command_button` shows a command's own
`enabled` reason. A test in `ui_ext.rs` fails on a new call site that drops it.

Labels and names (#1118): menus and buttons use sentence case ("Add note"). An ellipsis marks an
item that opens a dialog or asks for more before acting ("Export…", "Save project as…"), never a
submenu ("Export", "Save as", "Save to folder"). A glyph-only control ("…", "‹", "›") goes through
`Icon::icon_button` or `history::named` so a screen reader hears its name ("More for fox", "Previous
result, row 2"), and a tag marker carries its tag's name ("Tag: Hero shots") rather than its colour alone.
`a11y_check::unnamed_controls` finds any clickable AccessKit node whose name is empty or only
punctuation; kittests run it on the element library, a sheet and a tagged card.

Sizes (points at 100% zoom): grid spacing 20, dot radius 1 (fixed on screen), card radius 14,
panel radius 12, card padding 16, label gap 6, label 10.5 pt monospace, body 13 pt, handle radius 9, input port radius 5. Noodles are 2 points wide (3.5 selected, over a `card_stroke_selected` halo), at any zoom. Cards are 320 points wide. Ordinary parameter bodies use deterministic, control-specific row metrics plus padding, rather than a 220-point minimum plus 64 points per schema field. Longer bodies scroll internally at the 720-point parameter-body cap. The separate `composer_body_height` token reserves 220 points for a generation composer; `card_min_height` remains only a legacy deterministic creation-ID seed, not a card geometry input. Previews and answers retain their separate 180-point regions. Conditional controls keep their declared space so changing a value does not move ports and wires. Composer prompts, admitted generated-image count controls and hidden workflow-model fields do not reserve duplicate rows. Renderer, payload admission, navigation, formatting and creation geometry share the admitted composer roles; descriptor-only callers retain conservative bounds. Count ownership follows the contract meaning, independent of provider parameter names, current count or composer visibility. Control gaps are 6 points; `control_vertical_padding` is 2 points so native button rows agree with the logical metrics. Image actions reserve their 180-point preview before the first live result, and PDF source previews reserve the same region. History uses two compact rows rather than an extra 64-point parameter row. Multi-port cards retain a port-spacing height floor without enlarging the parameter viewport; vertical hit cells keep adjacent ports independently selectable at overview zoom and during legacy-input previews. Sliders use the full content width, with an exact numeric input below; text inputs and choice buttons also fill that width. The composer's submit button is 28
points across; the generation toolbar is 32 high, 10 above its card; the image viewer opens 720 wide. A sheet's table has 32-wide checkbox and number columns and
120-wide model, aspect, references, result, status and button columns, the prompt taking the
rest but never less than 240; Row settings opens at the right edge five control rows down; its
review grid's result thumbnails are 160 across (`sheet_thumbnail`), each with its status badge in
the top-left corner (`panel_fill` behind the status colour) and an `accent` outline when its row is
selected; the Sheets panel lists canvases 320 high, with a 250-wide title field. The Layer Editor's layers and
properties panels are 250 wide; a dragged layer snaps within 6 points of a guide, and its rotate
handle sits 24 points above it. App mode's form is 640 wide, and its media and result
thumbnails 120 across. The Timeline editor shares the Layer Editor's panel width; its clip track
is 84 high under a 22-high time ruler, and a clip's edge trims within 6 points.
The ComfyUI servers panel's jobs-at-once text field is `comfy_jobs_width` (48) wide,
and its new-server name field is `comfy_name_width` (160) wide.
The batch items review (#92) opens `batch_review_width` (520) wide; its Cross grid draws each
result `batch_review_cell` (96) across, with the first batch's items as rows and the second's as
columns.
A card's image preview, and a text card's answer, take `card_preview_height` (180) above the
composer; a long answer scrolls inside it and can be selected and copied. Media wired into a text
card (images, a PDF's first page, clips and sounds) show along the top of that answer area as
`connected_thumb` (40) squares, in port then wire order, `control_gap` apart; pictures fill the
square from their middle, other media show their kind's name, and when they don't all fit the last
square counts the rest (`+3`; past 256 assets it says "or more"). Each wire shows what a run would
read from it: a source's (or an Element's) current value, else the upstream card's usable active
result; a batch's items, which exist only once a run plans them, aren't shown. Only the squares
drawn ask for a preview. The row takes room from the answer, so wiring media never changes
the card's size. While a wire feeds a
card's prompt the prompt is shown disabled, its label says it comes from connected text (on the
label's own row, so the card keeps its size), and its hover text explains that nonempty connected
text is used instead; the typed prompt is kept and comes back when the wire is removed. A
generation kind other than images is offered only when the catalog has a model for it. A media
card's overlay — its prompt, its variation counter and its submit arrow — can be hidden for a
clean board with `H`; that is view state, so the document, the card's size and its other
parameters are untouched, and a text card's prompt is never hidden because there it is the editor.

Tests in `theme.rs` keep body text ≥ 7:1 against cards, labels ≥ 4.5:1 against the canvas, and the
selected border more visible than the resting one, in both themes.

## Canvas interaction

| Input | Effect |
|---|---|
| Drag empty canvas (primary) or drag anywhere (middle or right button) | pan |
| Two-finger scroll / mouse wheel | pan the canvas (zoom about the pointer with *Scroll = Zoom*), over cards too. Scroll chains: the innermost region under the pointer that can still scroll that way takes what it can before reaching its limit, and the canvas takes the remainder. The chained regions are a multiline text field, a card's parameter body (outside the text field), a text card's answer and a text action's input/output preview rows; distances are in screen points, so content moves exactly with the finger at any zoom; a card with nothing to scroll, a body at its limit, and horizontal scroll over a vertical body all pan the canvas. A swipe belongs to where it started: one begun on the canvas keeps panning when the pointer crosses a card, until the gesture ends |
| Pinch, or ⌘/Ctrl + scroll | zoom about the pointer (10%–400%) |
| ⌘/Ctrl + `=` / `-` | zoom about the centre by 1.25× |
| ⌘/Ctrl + `0`, or click the zoom badge | 100%, centred on the cards |
| ⌘/Ctrl+F outside unrelated editors, popups and gestures | open/focus canvas search |
| Enter / Shift+Enter in search | next / previous result, wrapping after the scan completes |
| Escape in search | close search and retain selection |
| Shift+1 / Shift+2 outside editors and gestures | fit all / selected nodes |
| Shift+M, or Hide map / Show map | toggle the minimap |
| `H` outside text fields, popups, menus and gestures, and not a held-key repeat | hide or show the prompt overlays on media cards, for a clean board. Shift is allowed because that is how a capital is typed; any other modifier means another key. View state: the document, the revision and the undo history do not move, the card keeps its size, a text card's prompt stays because there it is the editor, and the letter stays with the field, menu or gesture that owns the keyboard |
| Click or primary-drag the minimap | center the canvas on that world point |
| Drag a card background | move through `node.move`, one undo step per gesture; click selects it |
| Click a card / Shift-click | select only it / add or remove it from the selection |
| Shift-drag empty canvas | box select: cards the box touches join the selection (live while dragging); Escape cancels the box |
| Click empty canvas, or Escape outside text fields | clear the selection (Escape closes an open menu or cancels a box select first) |
| ⌘/Ctrl+A outside text fields | select every node on the canvas |
| Drag from a card's `+` output | draw a noodle; compatible inputs get rings and an accepting hovered card gets a theme outline. Precise port hits keep priority. Elsewhere inside a card, drop onto its first free compatible input (a list may append), or the nearest compatible occupied input if all are taken. Connect with `edge.connect`, or replace the old noodle atomically with `edge.rewire`. One undo restores the graph; one output can fan out to many inputs. Refused card/overlay drops preserve a picked-up wire; only empty canvas disconnects it. See the [slot rule](decisions/2026-10-27-body-drop-wiring.md) |
| Drag from a connected input | pick up its (last) noodle: drop it on another input to reroute (`edge.rewire`), or on empty canvas to disconnect |
| Drag from an unconnected input | compatible outputs get a ring. Drop on an output to connect, or on empty canvas to choose a compatible upstream node. Its output side lands at the drop point; creation and connection are one `node.create_connected` undo step. Escape cancels without editing |
| Escape while dragging a noodle | drop it without changing anything |
| Tab to a port, then Enter or Space | connect from the keyboard (#1109). Ports are focusable buttons named for screen readers, such as **Image output of Generate 1** or **Prompt input of Text 2, connected to Text 1**, with an `accent` focus ring. Enter or Space starts a pending connection and moves focus to the first port on screen that would accept it, drawn as a noodle with rings on every accepting port. Tab / Shift+Tab or the arrow keys cycle those ports, left to right then top to bottom, using the drag's rules: same modality, list into list, no loops, no duplicates. Enter or Space connects with `edge.connect`, or `edge.rewire` when it replaces a single input's noodle, one undo step each, and focus goes back to the starting port. An output starts a new noodle, a connected input picks up its last one to reroute it (the choice starts where it lands now), and an unconnected input chooses an upstream output. Escape cancels without editing and is consumed, so it clears no selection and reaches no other handler; a pointer press, a text field or menu taking the keyboard, or undo removing an end also cancels it. Like a drag, it reaches cards drawn on screen |
| Shift+Delete or Shift+Backspace on a focused port | remove its connection with `edge.disconnect`: an input's last noodle, or an output's only one (an output feeding several says to focus the input instead) |
| Click a noodle / Shift-click | select it / add or remove it; box select also takes noodles it crosses. Every noodle is also an AccessKit node named **Image from A to B · reference** (its media, cards and the input it lands on) and marked when selected, so screen readers can list connections (#1109) |
| Delete or Backspace outside text fields | delete the selected nodes, noodles and field in one undo step (`canvas.delete`; a deleted field's cards stay) |
| `T` / `I` / `V` / `A` / `R` (no modifier, outside text fields) | quick-add a Text / Image / Video / Audio / Router node centred under the pointer and select it (`node.create`, one undo step; a quick-added router carries text, since `node.create` sets no config). Keys come from each kind's `quick_add`, so new kinds bring their own |
| Double-click empty canvas | the quick-add palette (#1121): with nothing typed it lists Recent (the last five chosen; view state, gone on restart), node kinds by the media they make (Text, Image, Video, Audio, 3D, Document, Other), Actions, then Commands, under `text_muted` headings. Typing ranks every entry together, best first, naming its section on the right. Each row has its modality's `wire_*` dot (`tag_identity_size`), its label, its shortcut right-aligned in monospace `text_muted`, and a one-line summary in `text_muted` from the kind's or command's `summary()`. A command that can't run now is listed greyed after those that can, its `enabled` reason in place of the summary; ↵ or a click on it does nothing. ↑↓ move and keep the highlighted row in view, ↵ chooses, Escape or a click elsewhere closes; scrolling keeps the canvas fixed while this or the Turn into menu is open. It opens at the double-click, or upward from it when there's no room below, inside the window less `navigation_margin`. A kind is created where you double-clicked. See the [decision](decisions/2026-11-08-palette-sections-and-summaries.md) |
| ⌘/Ctrl+C, X, V outside text fields | copy / cut / paste the selected nodes and the noodles between them through the OS clipboard (versioned JSON, so another project or window can paste them); paste lands centred on the pointer with fresh ids and selects the result, one undo step (`canvas.paste`). Generation results are not copied. Plain text pastes as a Text node; recognized ComfyUI workflow JSON pastes as a workflow card after the server's nodes have been read, with the same validation as a file drop; a copied image pastes as an Image node at the pointer, with one undo step. Desktop pixels are read off the UI thread and encoded as PNG; browser PNG reads require clipboard permission. Copied native file references use the bounded file importer. Text editors keep ordinary paste; on the canvas typed native file references take precedence over their plain-text names (#319, #633) |
| ⌘/Ctrl+D outside text fields | duplicate the selection, offset 40 points |
| `F` (no modifier, outside text fields) | make a field: around the selected cards, which become its members, or an empty 320×240 zone at the pointer (`field.create`, one undo step). The zone takes in every selected card, including one added this frame. The new field is selected |
| Drag a field's header / its lower-right corner | move the field and every member (`field.move`) / resize the zone alone (`field.update`); one undo step on release, Escape cancels. Double-click the header to collapse or expand it. Shift-drag from a header or corner draws a box select, as anywhere else on the canvas |
| Right-click a field's header | rename, collapse or expand, fit to members, colour, or remove the field (its nodes stay) |
| Right-click a card → **Field** | move the selected nodes into a field, or take them out (`node.field_set`) |
| Right-click (or long-press) a card | menu: select the upstream / downstream chain of the selection, or all nodes of the card's kind; an unselected card becomes the selection first |
| Double-click the title | rename; Enter or blur commits, Escape cancels; empty restores the automatic title |
| A generation card's **runs fresh** / **reuses results** switch, beside its variations | turns result reuse on or off for that card (`node.cache_set`, one undo step, saved with the project, #1059): on, a run whose inputs and settings match a result already made takes that result free instead of asking the model again; model cards start off |
| Enter on a focused generation card, ⌘/Ctrl+Enter in its prompt, or its round arrow button | quote a run of that card (`run.quote`) and show its price, anything blocking it and any unknown part (a by-usage step whose provider can quote it is asked first, `run.estimate`, and the price shown again with its answer); **Generate for …** starts priced work (`run.start` with the quote's token and totals). The confirmation counts cached reuse as **N reused, free**; with only cached results, it says results are used where needed, already-active results stay in place, and nothing generates. Its button is **Continue without generating**: `run.start` applies any needed cached results but does nothing when they are already active. Cancel or Escape starts nothing. An edit after the quote makes it stale: nothing starts until it is quoted again. If the start fails, the dialog offers only Close, since the host may have taken the run. F2 or double-click still renames a generation card |
| ⌘/Ctrl+Enter outside text fields, or **Run changed** in the palette | quote a run of every card that changed or hasn't run (`run.changed`, mode `pending`): each generation, action and technique card whose result is stale, part-made or missing, with the upstream it needs, in dependency order (notes, decks, routers, batches and sources have nothing to run). The confirmation, titled **Run changed**, names the cards that run (**Runs N cards: …**), the total, anything blocking it and any unknown part, as a card's own run does; **Run changed for …** starts exactly the quoted cards (`run.start`, mode `pending`), and Cancel or Escape starts nothing. A card with work between two cards of the run runs too, in order. Cards after a batch the run feeds wait for its results; the dialog says so, and the next ⌘/Ctrl+Enter runs them. A card still running from an earlier run keeps the start button off until it finishes, so nothing is charged twice. When everything is up to date it says **Nothing to run** and opens no dialog. A focused field keeps its own ⌘/Ctrl+Enter (a prompt quotes its card; multiline text commits), and the key yields to drags, open menus, the palette, panels, dialogs and the recovery question. Free actions still recompute on their own when their inputs change, without a quote (#965) |
| **Run workflow** in the palette | the same, for the whole workflow (`run.workflow`, mode `workflow`): every generation and technique card, fresh or not, plus everything **Run changed** would run; a fresh action keeps its result. Never starts without the confirmation |
| Click a card's **Stale ↻** badge (shown when its result no longer matches its inputs) | quote a refresh of that card with whatever upstream is stale or missing (`run.quote`, mode `with_upstream`) and confirm it as above, in a dialog titled **Refresh** |
| Hover a card's **Can't check** badge (`danger`, shown on the first checked card `canvas.stale` could not hash) | the tooltip says its inputs can't be checked, names the failing upstream card by its title (or the card itself), the error kind and the engine's reason, bounded to 512 bytes with control and bidirectional characters blanked, and how many more results can't be checked. It offers no refresh, since a run would fail the same way; **List stale results** gives the full report |
| Click **Queue** (top-right, beside **API keys**; **Queue (N)** while N jobs are queued or running) | open the queue panel under search: every running and recent job with its card, provider and model, status, and price. **Cancel** stops a live job; **Retry** quotes a failed or cancelled card again like Generate; **Clear finished** forgets ended jobs. A cancelled job never sent shows **Not charged**; one that may have reached the provider shows its estimate as **unresolved**. Cancel asks the provider to stop; if it refuses or doesn't answer, the job shows **Cancel refused** and is followed to its end, since it may still be billed. A paid result that couldn't be kept on its card (it didn't fit its ports, couldn't be converted or couldn't be recorded) is **held** (#369): **Queue (N, M held)** counts them and the panel lists each with why, and **Retry**, **Put on canvas** and **Discard**; none calls the provider again. While 16 are held, new jobs are refused unsent. The desktop app saves held results so they survive a restart (#614). Cards show their own status under them (**Queued**, **Generating 40%**, **Run failed: API key**, with the full error and a hint on hover) |
| Click **Plugins** (top-right, beside **API keys**) | open the plugins panel (#172); in the browser it says packages last only for the session and can't run there (#823): every installed package with its version, state, licence, signer and grant. Type or **Choose…** a `.ncraft-plugin` file and **Check** it: a dry run shows who signed it (or that nobody did), its licence and the text of its licence and notice files, the IDs it adds and the capabilities and addresses it asks for (and, for an update, what is new), with nothing installed; **Install** or **Update** then imports it disabled. **Grant…** shows what a grant would allow, required capabilities ticked and fixed, optional ones to tick, before **Grant**; a data package says it needs none. **Enable**, **Disable**, **Revoke**, **Roll back to …** (a kept version, while disabled), **Uninstall** (asks to confirm) and **Remove unused versions** each dispatch one `plugins.*` command |
| `−` / `+` beside a generation card's prompt | step the model's image count (the parameter tagged `generated_image_count`) within its range |
| Select one generation card | a toolbar above it: the model (opens the model picker), the aspect ratio (the choice tagged `aspect_ratio`, or else one whose options are all `W:H`), and Expand once it has an image |
| Edit a parameter | schema-typed value through `node.param_set`; Enter or blur commits, ⌘/Ctrl+Enter for multiline text |
| ⌘/Ctrl+Z / ⌘/Ctrl+Shift+Z outside text fields | undo / redo document edits |

Shortcuts are spelled one way everywhere a person reads them (the palette, the Turn into menu,
tooltips, menus, notices and accessible names): `shortcut::format_shortcut` turns the engine's
`Command+Shift+Z` into `Shift+⌘Z` on macOS (Ctrl, Option, Shift, then ⌘; only ⌘ is a symbol,
since egui's default fonts have no ⇧, ⌥ or ⌃) and `Ctrl+Shift+Z` on Windows, Linux and other
browsers, in that fixed order. This page writes `⌘/Ctrl` for short; the UI never
shows that spelling (#1117).

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

The model picker (**Change model…**) groups catalog models by provider. After the catalog's
ComfyUI cards it shows a section headed **From ComfyUI server "name"** (#970). That section
lists what the default server's last read reports, by type: checkpoints, LoRAs, VAEs, text
encoders, diffusion models and upscalers. Only Generate image cards show it. The heading says the list comes
from the server, not the catalog, and **Refresh server models** reads the server again. The open
picker rebuilds the section, keeping the search text, when a read lands or the default server
changes; a list that can't be read says why in the heading row, in the `danger` colour. Entries
the app can't run stay in the list, disabled, with the reason on the row. A runnable entry whose
fit is a guess shows its warning instead of the workflow it runs through. The arrow keys and
Enter reach server rows as well as catalog models. A
checkpoint on the card that the server doesn't list shows as **Missing on the server**, in the
`stale` token colour, and is never swapped for another. Choosing a checkpoint goes through the
usual review step, and the review names the checkpoint. **Imported ComfyUI workflows** lists the
document's image workflow cards by title. Choosing one places a copy beside the card, `card_width +
layout_gap` to its right, and leaves the card itself unchanged.

Router connection menus keep their upstream output shape: a list stays a list through a chain
of Routers, and several incoming values offer only consumers that accept a collection. A Router
without a resolved source explains what to connect before offering downstream node kinds. Model
choices and wired-mode controls follow the resolved sources too; inserting a Router does not
make an incompatible model available. These menus do not fetch additional parameter options.

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
  worth reading together. #203's first 1-pixel-per-point replay run, 1.85 ms (200 nodes) and 6.21 ms
  (1,000 nodes), is a set of **release-mode move frames**, not `node.param_set` frames: its 60-frame
  "after an edit" loop dispatched `node.move` on successive nodes. Its grid shape, 10% zoom,
  moving pointer, egui-plus-tessellation span and pixels-per-point settings match the present
  move control, but the parameter-edit command and live-recompute work are different. Neither
  a quiet parameter-edit result nor these loaded runs can be interpreted as a before/after
  regression against #203's *parameter-edit* baseline, because #203 has no parameter-edit run. The #203
  report also did not record its macOS version or 1/5/15-minute load averages, preventing a
  load-matched historical comparison. A quiet re-measure is tracked in #743. The spread matters
  as much as the figures: runs taken earlier the same day, while other builds held the load
  average at 40-55, came out up to 3× worse
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
The Comments entry shares the application row with Search; they are the last entries to move into More when the row does not fit (#1115). Its discussion panel opens below the row, or below the search panel when search is open. The panel stays clear of bottom navigation when there is room; in cramped viewports it may overlap, with foreground discussion clicks explicitly excluded from minimap-pan admission. Since the map hides itself in short canvases (#1115), the panel no longer overlaps the map. Tags stay on the left; with both panels open their widths share the viewport. Node-attached markers overlapping a card or its output handle are displayed to the card's right with the bubble's ring and gap outside the actual bounds. At viewport edges, projected markers try the left, above or below the card while keeping their full ring reachable and clear of both port-hit strips. A viewport-filling card can use a safe interior position; if no safe position fits, protecting wire interaction takes priority over pin visibility. New node-attached anchors use zoom-independent world offsets, and markers follow live field-drag previews. Existing safe off-card offsets and free-canvas pins stay where placed. These display adjustments never rewrite stored anchors. Minimap admission uses the discussion panel's actual painted bounds, so unused space below a short panel remains interactive. A selected marker behind the Comments panel moves beside it with a leader; its stored anchor stays fixed. Pin dragging retains that display offset through the gesture, so release publishes only the actual world-space drag delta.

## Minimap and fit navigation

The bottom-right minimap shows actual instance card bodies, highlights selected nodes, and outlines
the current viewport. Its toolbar keeps Fit all, Fit selected, the map toggle and the existing zoom
reset discoverable. Map visibility, pan and zoom are session-local view state, never project data or
undo entries. Default map size is 320×160 points, with a 30-point toolbar, 16-point outer margin
and 6-point gaps; these dimensions and the 24-point fit padding are theme tokens.

In smaller windows (#1115) the panel takes a third of the canvas width (`navigation_fraction`),
between `navigation_min_width` (240 points, room for the toolbar's labels) and `navigation_width`,
and the map keeps the 320×160 shape. Each toolbar button gets its label's width plus an equal share
of what is left. Below a canvas of `minimap_hide_width` × `minimap_hide_height` (720×480 points)
the map hides itself, leaving Fit all, Fit selected and the zoom badge; Hide map is left out then,
since there is no map to hide. Shift+M still records the choice for when the window grows. At the
desktop's smallest window (640×400) and at 800×520 the panel covers at most a quarter of the canvas
(`small_windows1115_tests`).

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

The `canvas.fit_all`, `canvas.fit_selection`, `canvas.minimap_toggle`,
`canvas.prompt_overlays_toggle` and `canvas.pan_to` commands use the engine service path. Live hosts use `NodeCraftApp::dispatch_navigation(ctx, id,
params)` (and `navigation_enabled`) to compute current geometry, selection and focus, temporarily
install the capability, dispatch, restore prior resources and apply the intent immediately. A raw
headless Session has no live capability and returns Unsupported. Existing services/resources are
preserved. The older 100% reset button and shortcut retain their existing view-only path.

The quick-add palette also offers enabled navigation commands that need no parameters, including
Fit all nodes, Fit selected nodes, Toggle minimap and Toggle prompt overlays. Its search field lends focus only to these palette
choices; other editors, popups and active gestures still prevent navigation. Choosing a row
releases the palette query and uses the same live command bridge, without editing the project or
its undo history. Commands requiring coordinates, such as Pan to world point, remain absent. One
that can't run now, such as Fit selected nodes with nothing selected, is listed greyed with its
reason (#1121).

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

Browser file acquisition expires after ten seconds on the next executing frame. Escape cancels
an active browser drop when another editor or canvas gesture does not own it. Owned FileReaders
are aborted on cancellation, document replacement or timeout; late permission results cannot add
nodes. A still-open clipboard permission request remains the only request until it settles, so
repeated paste does not accumulate prompts. A browser drop retains at most 32 files, 16 MiB per
file and 48 MiB of Rust byte buffers. These bounds exclude browser-owned buffers and synchronous
engine decoding. Clipboard and drop delivery capture session identity, revision, canvas and
placement, then invoke the same engine importer once; one undo removes the batch.

## PDF reader

The PDF reader (#590) is a window over one PDF: thumbnails `reader_thumb_width` (112 screen points)
wide on the left, the page on the right, zoomed from 50% to 400%. Pages have a 1-point `card_stroke` edge so a white
page shows on a light window; the current thumbnail is outlined in the accent colour. Dragging over the page selects a rectangle drawn with `marquee_fill` and
`marquee_stroke`, the canvas's box-select tokens; "New Text node from selection" reads the text inside it.

## Agent sidebar

The agent sidebar (#114, Cmd+/) is a right panel that opens `agent_width` (360 screen points) wide;
the canvas takes the rest of the window. Its left edge drags it between `agent_min_width` (280
points) and `agent_max_fraction` (half) of the window, so a small window keeps half its width for
the canvas (#1115); the sidebar never goes below its minimum. Its header shows the mode (Assist/Auto) and Clear, then a muted line
with the model and what auto mode has accepted of its budget; notices use `danger`. Steps waiting
for approval show what they would do (read, change the project, run and its price) with Approve
and Decline; Esc or Cancel stops the agent while it works.

## Local text action previews

Join text, Template, Split text, Replace text and other Action descriptors with only Text
outputs show their computed output and connected inputs on the card. Each input names its port,
ordered slot and source; routers retain the planner's feed ordering. The preview reads existing
results, and refreshes its cached summaries only after document changes. An older result is
labelled updating while the next live computation runs.

The two sections together reserve twice `card_preview_height` (2 × 180 points), independent of
result content. Each summary is limited to `preview_chars` (256 characters) and three displayed
lines; a virtualized list scrolls longer input collections. Empty text/list placeholders differ
from a missing output. Missing connections and action errors appear inline, with the input port
and the theme's `danger` colour. Fixing a parameter or input removes its obsolete error.

Each available value has an Expand action. Its modal lays out at most the first 64 KiB of text,
cut at a Unicode character boundary. A notice gives the displayed and retained byte counts;
Copy full text copies the retained value on demand, within the inspector's 8 MiB/16,384-value
limit, without editing the project. Escape or
Close text preview dismisses it; canvas shortcuts wait while it is open. Both themes use the
existing prompt/text/spacing tokens, and local previews follow the ordinary small-zoom body
threshold. Cache admission limits and rationale are in the
[decision](decisions/2026-10-26-local-text-previews.md).

## Application menu

The application row (#956) uses one icon-and-label control style for File, Tag filter, Sheets,
Techniques, Elements, Queue, Share, Budgets, API keys, ComfyUI, Plugins, Search and Comments.
File and Tag filter lead the row (#1123); navigation, activity and settings/services follow,
separated by dividers, with Search and Comments on the right.
Browser server settings add Providers, and a selected Technique adds Open as app. Every entry
uses the same token-based height, padding, hover, active and disabled styling. Queue counts
appear beside its label and remain in its accessible name; unavailable services stay disabled.

If the measured row does not fit, entries move into one More menu from the row's right (#1115):
first the navigation, activity and service entries from the right, then the entry of an open panel
(kept in the row while others go), then Tag filter, then Comments, then Search, and File last of
all (#1123): it is the only way to open and save (`Entry::rank`, pinned by
`small_windows1115_tests::fit1123_*`). More sits after the entries the row
keeps, before Search and Comments, and lists only what moved, with the same icons, states and
order. File, Search and Comments stay in the row down to the desktop's smallest window (640 points) while the agent sidebar is closed; with it open there, Search and Comments move into More too. It scrolls in short viewports; keyboard focus follows visual order and skips disabled
entries. More yields to active text editors and canvas gestures; pressing a disabled entry while a
text editor has the focus leaves the focus there. Format selection keeps a slot at the row's left
while several cards are selected.

**File** opens the File menu below it (below More once File has moved there): Open…, Open
recent ▸ (the recent projects, each with Remove, then Clear recent projects), Save, Save as…,
then Save workflow results… and Import folder…. Shortcuts show on the right as this platform
spells them, and in each entry's accessible name ("Open… (Ctrl+O)"). The entries run the same
flows as the shortcuts and the palette (`file.open`, `file.save`, `file.save_as`, the results save,
the folder import, opening a recent project). An entry that can't run now stays listed, disabled,
and says why on hover; without project storage the reason also heads the menu, with Focus the
other window when another window owns storage. The menu is `file_menu_width` wide, it and its
Open recent list scroll in a short window, and it closes on Escape, when an entry is chosen, or on
a click outside, which never reaches the canvas (a click on another top-bar entry or a panel's
control still acts, as before). The browser's File menu offers Open…, Save and Save as…, with
the same rules: while it is open the canvas gets no keys or clicks.

**Tag filter** opens the movable, resizable tag panel below the row, and closes it again. Its centered header holds Close, followed by a name search, color-marked tag checkboxes with card counts, Untagged, and match navigation. While a filter is active the
entry shows the match count beside its label and in its accessible name ("Tag filter (3
matches)"). Sharing status is
available on Share's tooltip and collaborator avatars are in its panel. See the
[layout and ownership decision](decisions/2026-10-27-unified-top-bar.md) and the
[small-window decision](decisions/2026-11-08-small-window-layout.md).

## Starter templates in the Techniques panel

The Starter templates section (#953, #954) heads its list with two buttons: **Update templates**
(`templates.update`) and **Clear downloaded** (`templates.clear_cache`, disabled until something is
downloaded). Below them one small `text_muted` status line says what the last update did ("Using
the bundled templates…", "2 templates were downloaded and verified.", how many were rejected or
left for the next update), or shows a spinner with "Updating templates…" while one runs; Update is
disabled meanwhile. A damaged cache shows one `danger` line beginning "Template cache:" with the
first problem and a count of the rest. Windows that can't download (the CLI has no panel; a host
without the fetch service) disable Update and say "Bundled templates only". Nothing in the panel
touches the network until Update is pressed.

## Library browsers

The Techniques panel (#96) has four tabs: **Library** (published techniques, the shared
technique and shader libraries, starter templates), **Assets**, **History** and **Styles**. Each
browser shows a page of 20 entries with Previous and Next, read with a read-only command when
the tab opens and again whenever the document or a filter changes, so a large project never
draws more than a page.

- **Assets** (`library.assets`) lists every distinct image, video, sound, 3D model and PDF the
  project's cards hold or made, once each, with how often it is used. Images and PDFs show a
  `library_thumbnail` (56-point) thumbnail through the shared preview cache; other kinds show
  their kind's name in the same square. Kind chips (All, Images, Videos, Sounds, 3D, PDFs) and a
  search field filter it. **Insert** adds a source card holding the asset in the middle of the
  view, selected, as one `node.add` undo step.
- **History** (`library.history`) lists the open canvas's past results, card by card and newest
  first, with the first image's thumbnail or a quote of the text, the model, and whether the card
  uses it now. **Use this result** restores it (`history.select`); **Place** puts its value on the
  canvas as a source card in the middle of the view (`history.extract`). **Selected card only**
  narrows it to one card.
- **Styles** (`library.styles`) lists every Train style card on every canvas: its family and
  trigger, a **Soul ID** mark (`accent`) for a trained identity, and "Kept by provider": the
  weights or the identity stay with that provider and only that account can use the handle. A
  Soul ID instead says it is bound to the Higgsfield account that trained it and is not exported,
  and, against the account whose Soul IDs were last fetched, "Unavailable on this account"
  (`danger`) or "Not checked on this account yet" (`text_muted`); a Soul ID whose descriptor is
  missing says so and to train it again (`danger`). A muted line counts the SOUL 2 cards that name
  a Soul ID picked from the account's list, even when no Train style card exists.
  **Use on selected card** wires a style on the open canvas into the selected card
  (`style.use`), or sets a SOUL 2 card's Soul ID, disabled with the reason when the card can't
  take it; a style on another canvas offers **Open its canvas**. The **Share live** panel adds a
  muted note when the project uses Soul IDs: others see them, but runs on another account are
  refused. A technique in the **Library** whose cards name a Soul ID carries a "Uses a Higgsfield
  Soul ID" note: it runs only on the account that trained it, in any project.

**Ticking entries.** In Assets and History, each entry's title is a tick box; up to 32 entries
per tab can be ticked across pages (the ticks are forgotten when the project or canvas changes,
and an entry the document no longer holds is unticked). A bar
above the list then says how many are ticked and offers **Place on canvas** (`library.attach`:
every entry as a source card in a grid centred in the view, selected, one undo step, each card
titled after where it came from), **Export…** (the workflow-result save with the ticked results
as targets, after a folder is picked; disabled for an asset that is a card's own file rather
than a result on this canvas) and **Clear ticks**. A disabled button says why on hover.

**Import files…** (Assets, desktop only; the browser build says to drop files on the canvas)
opens the system's file picker. The files are read on their own thread while a foreground
panel at the bottom left shows "Reading files for the library… 2 of 5" with a progress bar and
**Cancel import** (then "Checking the videos' lengths…" while a video without a stated length
is measured); the import then adds a source card per new file in the middle of the view,
selected, as one undo step, titled with the file's name. A report above the list gives the
summary ("2 added · 1 duplicate · 1 failed") and a line per file, failures in `danger`, with
**Retry failed (N)**, which reads only the failed files again (disabled while files are being
imported), and **Dismiss**.

**Import from URL…** (Assets, desktop only) opens a row with an address field, **Import** and
**Close**. The field says under it why an address won't do ("only https addresses are imported",
"addresses on this computer or its local network aren't imported") and Import stays disabled
until it will. While the file downloads, the same foreground panel shows "Downloading from
<host>…" with **Cancel import**; the file then takes the Import files… path and gets a line in the
report, titled with the address's file name. A failed download gets one `danger` line naming the
host, with nothing to retry. The browser build disables the button and says to download the file
and drop it.

**My library.** The Assets tab has two scopes, **This project** and **My library**. With entries
ticked, the bar also offers **Save to My library** (Assets and History), which copies the files
into the library every project shares and says what became of each in the notice ("2 saved to My
library · 1 already in My library"). The My library scope lists the saved files newest first,
with the same kind chips and search, a square naming each file's kind where a project asset shows
its thumbnail (the file isn't in the project until it is placed), "Image · from <project> ·
1.4 MB", and **Place** (a card in the middle of the view, copied into this project, one undo step)
and **Remove from My library**. Ticked entries get **Place on canvas** and **Remove from My library**. Above the
list, "12 files · 34.0 MB of 1.1 GB" and **Refresh** (to see what another window saved). An index
that can't be read is explained in `danger` with **Repair**, which rebuilds the list from the
saved files and says in the notice what it recovered and where the old index was kept. With more
entries ticked than one request takes, the bulk buttons are disabled with the reason. In the browser My library lasts until the page is
closed, and says so ([decision](decisions/2026-11-03-library-url-import-and-global-library.md)).

## First run and the empty canvas

An empty canvas shows a quiet panel in its middle (#1119): **Double-click anywhere to add a node**
with an **Add a node** button that opens the quick-add palette there, **Drop images, video or a
folder here**, and **Open a Technique…**, which opens the Techniques panel. While the canvas holds
only the sample Text card with nothing wired, a tip beside it says **Drag from + to connect to a
model** until it is dismissed or something is wired. With no API key and no ComfyUI server set up,
a generation card's submit tooltip and the run quote say so, and the quote offers **Open API keys**
and **Open ComfyUI**. All of it is view state drawn by the shell: the CLI, MCP and control channel
never see it, and a shared-room viewer who can't add nodes gets no prompt to.

## Interface size

The interface size (#1107) draws the whole shell larger or smaller, from 80 %
to 200 % (default 100 %). It is a setting of this installation, not of a project:
`[appearance] interface_size = 150` in `config.toml` on the desktop, this origin's
`localStorage` (`nodecraft.interface_size`) in the browser. The shell applies it as egui's zoom
factor (`Context::set_zoom_factor`), so every token in points (text, spacing, radii, panel widths)
grows together and nothing needs its own scaled size. A value the file holds that isn't a number
from 80 to 200 reads as 100 %, is written back as 100 %, and the startup notice says so; the rest
of `[appearance]` keeps its values.

Commands, which the palette, shortcuts, control channel and MCP share: `view.ui_scale.increase`
(⌘⌥= or ⌘⌥+, Ctrl+Alt elsewhere) and `view.ui_scale.decrease` (⌘⌥-) step by 10 % to the next
multiple of ten; `view.ui_scale.reset` (⌘⌥0) returns to 100 %; `view.ui_scale.set {percent}`
takes any number from 80 to 200, rounded to a whole percent, and refuses anything else. Increase,
decrease and reset are disabled when they would do nothing (at 200 %, at 80 %, at 100 %); a
damaged value counts as 100 % once the shell has saved the clean value on its first frame. Each
returns `{percent, previous}`. They record no document change and no undo step. They are
live-only: the control channel routes them to the window, and without one (`nodecraft-cli`, a
headless session) they are unsupported. The shortcuts match the key's logical character; only `0`
also matches its physical key, as the other digit shortcuts do, so an AltGr character on the `-`
or `=` key (Ctrl+Alt is AltGr on many layouts) never changes the size. They are taken before the
canvas reads its zoom keys, so Ctrl+Alt+= never also zooms the canvas, and never while a text
field has focus, where AltGr types a character. On macOS, ⌥⌘= and ⌥⌘- are also the system's
Accessibility Zoom shortcuts when that feature and its keyboard shortcuts are turned on; macOS
then takes them first, and the palette entries still work.

Canvas zoom is unchanged and separate: the zoom badge still reads the canvas zoom, and cards,
which are drawn in points like everything else, grow with the interface the way a browser's page
zoom grows a page ([decision](decisions/2026-11-08-interface-size-is-a-zoom-factor.md)). Previews
are requested at `pixels_per_point`, so they stay sharp. At 200 % a 1280×720 window is 640×360
points: the top bar overflows into More (#1115), floating panels are clamped to the panel area,
and questions (#1116) fit, which a test checks at 150 % and 200 %.

## Overlays and stacking

One rule decides how every surface over the canvas moves, resizes and stacks (#957). It is
implemented once, in [`crates/ui-egui/src/floating.rs`](../crates/ui-egui/src/floating.rs);
panels do not position themselves. See the [decision](decisions/2026-10-28-floating-panel-rule.md).

**Stacking, back to front** (egui `Order` in brackets):

1. **Canvas** (`Background`): the board, cards and wires.
2. **Floating panels** (`Middle`, `floating::PANEL_ORDER`): the last opened or pressed panel is
   in front. Opening one never reorders the others.
3. **Pinned chrome** (`Foreground`): the application row, Format selection, Search,
   Discussions, the minimap and comment pins.
4. **Menus, popups and dialogs** (`Foreground`, shown after chrome): the More and File menus, quick-add,
   Turn into, mention lists, egui menus and combo boxes, and question dialogs.
5. **Notices** (`Foreground`, bottom-left): the notice and staleness check, update banner,
   pending drop, folder import, library import and background export progress. They are above panels and
   chrome. The quick-add palette and Turn into menu are raised over a notice while open (2026-10-18
   staleness decision), so a menu the keyboard owns is never under a notice.
6. **Tooltips** (`Tooltip`): egui's own, above everything.

**Floating panels** are built by `floating::panel` and behave the same everywhere:

- **Header** (#1123): one style for every panel, drawn by `floating::panel` rather than egui's
  title bar (which centres the title): the title on the left at `body_size` in the `text` colour,
  the close button on the right, a `panel_stroke` divider below. egui's default fonts have no
  semibold face, so size, colour and the divider set the title apart.
- **Move** by dragging the header (title bar). Dragging inside the body never moves a panel.
- **Resize** from the right or bottom edge or the bottom-right corner, never larger than the
  panel area. Every panel is pivoted at its top-left corner, so the header and the top-left stay
  where they are while resizing; the left and top edges are not handles.
- **Dismiss** with the labelled close button in the header, which is focusable by Tab, or the
  control that opened it.
- **Panel area**: the window below the application row, and below an open Search and left of
  open Discussions, which are pinned chrome drawn above panels. A panel is clamped into it every
  frame, so dragging, a smaller window, a resized panel or opening Search or Discussions can never
  put its header under chrome or out of reach, and it can never cover the control that opened it.
- **Opening**: a panel opens where it was last left, or first at its home: its top-right corner
  at the panel area's top-right (`navigation_margin` in from the window edge); viewers open
  centred at the same height. Tag filter starts at the left navigation margin below the File control. A first opening is measured by egui unseen and placed on the next
  pass with its real size. If it would cover an open panel's header, it steps down by
  `panel_cascade` (44) until every open panel's title and close button stay visible.
- Position, size and stacking are view state in egui memory, never document changes or undo steps.

**Dialogs: questions and tools** (#1116, [decision](decisions/2026-11-08-dialogs-question-or-tool.md)):

- A **question** the person must answer before continuing (a confirmation, a rename) is a modal
  built by `floating::question`: centred over a dimmed window, in the shared panel frame
  (`theme::panel_frame`), no wider or taller than the window less its margins. **Enter** takes its
  primary action and **Escape** cancels; it reads both with `floating::answer_keys` before it is
  drawn, consuming them, and is drawn before any canvas handler, so neither key also reaches the
  canvas. Only a fresh press counts: key repeats never answer, and Enter answers only after it has
  been released since the question opened (an Enter held in the step before can't also answer
  it). With one of its buttons focused (Tab), Enter presses that button, so a focused Cancel
  cancels. A question whose only action loses work answers Enter with nothing (browser Unsaved
  changes: **Discard and open** takes a click). Clicking the dimmed backdrop cancels too. Screen
  readers see it as a `Dialog` named by its title.
- A **tool** the person works alongside is a floating panel (`floating::panel`); the fixed Shared
  parameters panel is `floating::pinned`. Escape closes the frontmost of Workflow fields and Batch
  items, taken before the canvas reads it, but only when no other Escape owner is active: a
  question, a popup, quick-add or Turn into, a text field (Escape leaves it first), the keyboard
  connect flow, Search, Discussions, the Tag filter, any drag or gesture, or a browser file read
  each take Escape first and the tool stays open. Choose a model closes on Escape too, and the
  File menu (#1123) takes Escape at the top of the frame, before any of these.
- Nothing uses a raw `egui::Window`: `floating.rs` is the only place that builds one (inside
  `panel` and `pinned`), and `floating::tests::no_raw_windows_outside_floating` fails on a new one
  elsewhere in non-test code.

| Surface | Kind | Moves | Resizes | Dismiss |
|---|---|---|---|---|
| Sheets, Techniques, Elements, Queue, Share live, Budgets, API keys, ComfyUI servers, Provider servers (browser), Plugins, Automation | floating panel | header | right/bottom edges, corner | close button or its row control |
| Tag filter | floating panel | header | right/bottom edges, corner | Close tags, Escape, or its top-bar entry |
| Custom shader, Image viewer, PDF reader, Charges to review, Batch items, Workflow fields, Choose a model | floating panel (viewer/review/tool) | header | right/bottom edges, corner | close button or Escape |
| Shared parameters (bulk) | fixed: pinned top-right while several nodes are selected; panels opening over it step below its header | no | no | change the selection |
| Application row, Tag filter trigger, Format selection, Search, Discussions, minimap | pinned chrome | no | no | its own control |
| Comment pins, generation toolbar, Save result toolbar | attached to their card or comment | with the canvas | no | deselect |
| More menu, File menu, quick-add, Turn into, mentions, context menus | menu | no | no | Esc or click outside |
| Model preview changed, Review model change, Rename field | question (`floating::question`), above panels | no (centred) | no | its buttons, Enter (primary), Escape or the backdrop (cancel) |
| Unsaved changes (browser) | question (`floating::question`), above panels | no (centred) | no | its buttons; Escape or the backdrop cancels; Enter does nothing |
| Recovery, run quote, export and the other modals | modal, above panels | no (centred) | no | their buttons only (Escape where noted); Enter is never wired to a paid run |
| Notice | notice stack (#1110) | no | no | its close button, Escape while it holds the focus, or a timeout (not errors) |
| Update banner, pending drop, folder import, library import, background export | notice | no | no | its own button |

**Notices** (#1110, [decision](decisions/2026-11-08-typed-notices.md)) are view state in
`notices.rs`: a stack in the bottom-left corner, the newest three shown (`MAX_NOTICES`). A new
notice always goes on top; room is made by dropping the oldest passing notice, never a sticky
one, and the ones it covers are counted on a "N more notices below" line (sticky ones bounded at
32). The same text again refreshes the one shown instead of stacking a copy. Each is an **Info**, **Success**, **Warning** or **Error** notice in the
shared panel frame, `notice_width` (380) wide or the window less its margins, with a
`notice_edge` (3) stripe down its left side and an icon in its kind's colour (`text_muted`,
`accent`, `stale`, `danger`; Tabler info-circle, circle-check, alert-triangle, alert-circle), so
colour is never the only cue. Its text is `text`.

- **Errors and run warnings stay until dismissed** (sticky); the others go after 6 s on screen,
  counted in egui time. The clock stops while the pointer is over a notice or one of its buttons
  has the keyboard focus, and while the stack isn't drawn.
- **Dismiss** with the close button (**Dismiss notice**), or **Escape** while that button (or the
  notice's action button) has the keyboard focus and the stack was drawn the frame before. The
  notice takes Escape early, with the tools (`take_tool_escape`; on a sheet, before its table),
  and only when no other Escape owner is active; without the focus, Escape
  goes where it did before (it clears the selection).
- An optional **action** button runs a command id as the palette would.
- Each notice's text is an AccessKit **live region**: polite for information and successes,
  assertive for warnings and errors, described by its kind.
- Engine errors map to a kind in one place, `NoticeKind::of_error` (a `Busy` refusal is a
  warning; provider, budget, key and network failures are errors), and run events in
  `NoticeKind::of_run`. Code reports with `notices.report(error)`, `error`, `warning`, `success`
  or `info`. `clear`, after an action has succeeded, removes only passing notices; a sticky one
  goes when dismissed, or when the code that raised it with an owner (`report_for`, `error_for`)
  settles it: a card's edit, a connection, a save or an export that later succeeds, or any
  document change for an id allocation or canvas-space error.
- Views without the corner (a deck being edited or presented, an app form) show the newest as one
  line with `notices::inline_notice`. An error bound to a control (a card's parameter, the model
  picker's choice) is `notices::inline_error`: the error icon, then `danger` text.

The generation and Save result toolbars draw in the panel order while their card is selected;
they follow the card, so they are not clamped or cascaded, except that the generation toolbar of a
card under the application row stays below the row (#1115).

## Menu and panel inventory (#959, in progress)

The shared check is `panel_check::check_open_panels`
([`panel_check.rs`](../crates/ui-egui/src/app/panel_check.rs)), run by
[`menus959_tests.rs`](../crates/ui-egui/src/app/menus959_tests.rs) for the top bar and by each
service-backed panel's own test once that panel is open. It finds a panel's controls through
that panel's own accessibility node, by id. Each top-bar floating panel opens as exactly one panel inside the floating panel area (below the top
bar, clear of open Search and Comments), at 1000 × 700 and 1600 × 900, in both themes. None of its
on-screen controls (buttons, fields, checkboxes, combo boxes, sliders, links) crosses another (a
control and its own accessibility descendants aside). Clipping isn't checked: egui's accessibility
tree doesn't mark scroll areas, so scrolled-away content can't be told from cut-off content. Choosing its entry
again closes it. Only the state a panel opens in is checked, nothing scrolled or expanded.
Questions (`floating::question`, #1116) get `panel_check::check_question`: open, on the screen,
and no crossing controls inside their `Dialog` node.

| Surface | Opens from | Kind | Covered by |
|---|---|---|---|
| Sheets, Techniques, Elements, Budgets, API keys | top bar | floating panel | `menus959_tests` (open, area, overlap, close) |
| ComfyUI servers, Plugins | top bar (disabled without their service) | floating panel | `menus959_tests` (opened through their state), `topbar_tests` (disabled state) |
| Queue | top bar (disabled without runs) | floating panel | `held_restart_tests` (with the layout check) |
| Automation | top bar (with an MCP host) | floating panel | `automation::tests` (with the layout check, server running) |
| Charges to review | palette `usage.ownership` | floating panel | `ownership_panel::tests` (with the layout check) |
| Custom shader | a shader card's menu | floating panel | `shader_editor_tests` (opened at 1000 × 700 and 1600 × 900, with the layout check) |
| Share | top bar | floating panel | `sharing::tests` (with the layout check, idle and live, at 1000 × 700 and 1600 × 900) |
| PDF reader | a PDF card's menu, Read PDF… | floating panel | `reader_tests` (with the layout check) |
| Image viewer | the toolbar's Expand | floating panel | `runs_tests` (with the layout check), `preview_tests` (a tall image at 1000 × 700 and 1600 × 900) |
| Batch items | a fanned card's menu, Review items… | floating panel | `batch_review_tests` (with the layout check; at 640 × 400 with Escape) |
| Workflow fields | a ComfyUI workflow card's menu, Edit workflow fields… | floating panel | `comfy_mapping::tests` (with the layout check; at 640 × 400 with Escape) |
| File menu | top bar, File (first entry) | menu | `file_dialogs::tests` (`panel_check::check_menu` at 1000 × 700 and 1600 × 900; Open recent, Remove, Clear; disabled with reasons), `small_windows1115_tests` (640 × 400: below File, Open recent lists entries, Escape), `ui_scale_tests` (150 % and 200 %, with Open recent) |
| Tags panel | top bar, Tag filter | pinned chrome | `small_windows1115_tests` (opens below the row, closes, the count badge) |
| Choose a model | a card's Change model… | floating panel | `models::comfy970_tests` (with the layout check, while searching), `models::tests` (640 × 400, Escape; a failed choice shows at its top) |
| Shared parameters | selecting two or more cards | pinned panel | `small_windows1115_tests` (on screen at 640 × 400, goes when Escape clears the selection) |
| Rename field | a field's menu, Rename field… | question | `fields_tests` (with the question check; at 640 × 400, Enter saves, Escape cancels) |
| Review model change, Model preview changed | the model picker, after a choice | question | `models::tests` (with the question check; at 640 × 400, Enter applies or refreshes, Escape cancels) |
| Unsaved changes (browser) | File, Open… over unsaved changes | question | `web_files::tests` (on screen at 640 × 400, Escape keeps the project, Enter does nothing) |
| Search | top bar, Ctrl/⌘+F | pinned chrome | `menus959_tests` (open, Escape), `topbar_tests` (focus) |
| Comments (Discussions) | top bar | pinned chrome | `menus959_tests` (open, close) |
| More (overflow) | top bar at narrow widths | menu | `topbar_tests` (order, icons, disabled, Escape, focus), `small_windows1115_tests` (what moves, at 640 × 400, 800 × 520 and 1440 × 900) |
| Quick-add, Turn into | canvas | menus | `palette_scroll_tests`, `palette1121_tests` (sections, ranking, unavailable commands, on screen from the bottom-right corner at 1440 × 900 and 640 × 400), `turn_into_tests` |

Since #1116 there are no windows outside `floating.rs`: questions are modals sized to the window,
tools are floating panels clamped to the panel area.
`small_windows1115_tests` opens Techniques, Budgets, API keys and the File menu at 640 × 400,
800 × 520 and 1440 × 900 with the layout checks.

**Outcome so far.** Every panel in the first two rows passes at both sizes and in both themes.
The API keys panel sits past the window's right edge for its first placement frames at 1000 px
and settles inside once laid out; the checks run after placement settles. The image viewer ran
past the bottom of the window for a tall or square image; it now shows the image smaller, keeping
its shape, so the panel fits (`floating::body_room`).

**Not yet reviewed or covered:** Provider servers (browser only), every modal (app mode, deck editor and presenter, tag
definition, export, results save, file progress, unsaved-changes confirmation), the recovery dialog,
the update banner, card and field context menus, the inspector and bulk panels, notices, and the
Tag filter (#958). The issue's per-surface checklist (focus order, Enter, empty and disabled
states, tokens), before-and-after screenshots, and Escape closing a floating panel are open too.
The dialogs migrated in #1116 consume their Escape (each test checks the canvas selection
survives it); the remaining top-bar panels still close only by their button, since several canvas
handlers read Escape without consuming it.

The [9 November 2026 audit](reviews/2026-11-09-ui-ux-audit.md) extends this index to the native
Agent sidebar and every direct modal, records new narrow-window and AccessKit findings, and
separates existing tests from new probes and unverified states. It also reconciles #1125's stale
checkboxes against the issue states at the reviewed snapshot. #959 remains open until its fixes,
per-surface acceptance checks and before/after evidence are complete.

The [layout repair record](reviews/2026-11-09-ui-ux-layout-repairs.md) describes the bounded Mask
editor, scrollable provider settings, constrained Layer Editor title and scrollable run-quote
details, with pointer/key regressions in both themes and at 200% interface size.

## Saving results

Generated media previews and their card menus offer **Save as…** and **Save to folder…**
(#967). A gallery thumbnail targets that exact output and flattened batch item; a card with
several outputs offers a choice. An open menu keeps the historical result that was clicked,
even when a new run finishes. A transient live-action preview cannot be saved as a finished
result. With no saved media, the disabled actions explain how to obtain one.

The selected card's hover toolbar offers Save result; generation cards retain Download.
These actions share the existing `history.export` background file writer and its native dialog,
which suggests the stored media's extension. Saving one file writes no siblings or sidecar and
does not edit the document or history selection. Save to folder passes the same exact historical
selector to the workflow-result preview, manifest and worker (#966). Its existing secure folder
writer currently supports macOS and Linux; other platforms explain the limitation before a
folder can be chosen. Individual Save as remains available on desktop Windows.

An **Export to folder** card (`media.export`, #746) is a sink: wire image, video, audio, 3D or
document results into its ports, and its generated body shows the file-name pattern, what to do
when a file exists (keep, number or replace), the format and an optional subfolder. Above them the
card shows the chosen folder's name ("No folder chosen yet" until then), **Choose folder…** (the
native folder dialog; the only way to grant a folder) and **Export**, enabled once a folder is
chosen. The folder is the window's view state for that card and never enters the project. Export
writes on the background export worker behind a progress window with **Cancel export**; when it
finishes, a notice says how many files were written, kept or failed and why. Running the canvas
never exports. The browser build and non-Unix desktops show why export is unavailable instead of
the buttons.

## Control icons

The Tag filter uses Tabler x, filter-off, chevron-left and chevron-right for close, clear and
match navigation. Controls use Tabler outline SVGs through `icons::Icon`, prepared by the pinned Craft SVG
catalog and loaded by egui's SVG loader. The loader rasterizes at the requested display pixel
size on desktop and WebAssembly. It loads only embedded bytes; no network or filesystem access
is enabled. Source files stay unchanged under `assets/icons/tabler/`, with the MIT license beside
them and one attribution row per file. See the [choice and comparison](decisions/2026-10-26-tabler-control-icons.md).

| Icon variant | SVG | Initial use |
| --- | --- | --- |
| `Search` | `search.svg` | canvas Search button |
| `Comments` | `message-circle.svg` | Comments button |
| `Key` | `key.svg` | API keys button |
| `Wallet` | `wallet.svg` | Budgets button |
| `Open` | `folder-open.svg` | Open project and Import folder |
| `Save` | `device-floppy.svg` | Save project and Save project as |
| `Table` | `table.svg` | Sheets |
| `Technique` | `affiliate.svg` | Techniques |
| `Elements` | `components.svg` | Elements |
| `Queue` | `list-details.svg` | Queue |
| `Share` | `share.svg` | Share |
| `Server` | `server.svg` | ComfyUI |
| `Plugin` | `puzzle.svg` | Plugins |
| `More` | `dots.svg` | application menu overflow |
| `Apps` | `apps.svg` | Open as app |
| `Cloud` | `cloud.svg` | browser Providers |
| `File` | `file.svg` | the top bar's File entry |
| `Tag` | `tag.svg` | the top bar's Tag filter entry |

Use `Icon::button` for an icon beside action text; it retains the ordinary Button label,
keyboard behavior and disabled state. For an icon-only control use `Icon::icon_button`, which
requires an action label and supplies both its screen-reader label and hover tooltip (an empty
label falls back to the icon's action name). Standalone images also carry an alternative label.
Buttons keep their existing enabled conditions, commands and action tooltips.

The `icon_size` and `icon_large_size` tokens are 16 and 24 screen points. `icon_stroke` is `"2"`
in the source SVG's 24-unit coordinates; it is textual because the shared SVG preparation API
substitutes that exact attribute. Both themes share this stroke. The intermediate white mask
is tinted with the theme's `text` colour at draw time, including inside disabled controls.
Never add a coloured icon asset or bake a fixed-resolution icon bitmap into a control.
