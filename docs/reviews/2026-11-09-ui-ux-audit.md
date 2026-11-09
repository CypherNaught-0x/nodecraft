# UI/UX audit — 9 November 2026 (#959)

This audit found five follow-up defects in layout and accessibility and one source-inspected
accessibility gap in the native Agent sidebar. It preserves the distinction between a reviewed
surface, a tested opening state and a completed acceptance checklist. **#959 is not closed by this
report:** several modal states, native/browser runtime checks and fixes below still need work.

## Evidence and limits

The source snapshot is `caecbb32c8fdc9b6f9034bc4507f5c1f35f865b6` (verify against the audit's
`base-commit.txt` when reproducing). The review read the UI design, architecture and contributor
rules, #959, #1125 and the open PR inventory before assessing findings.

Tests use real egui pointer/key events, AccessKit trees and wgpu renders of public fixtures.
The established top-bar inventory tests passed (3 tests), as did 93 targeted theme, shell,
palette, question, field and PDF-reader tests. Of 45 enabled editor/form/browser-settings behavior tests,
44 passed in the initial concurrent run; the Timeline export test collided with another export,
then passed when run alone. One installed-FFmpeg-dependent test was ignored. This timing result
is recorded as a test limitation, not a new product finding.

Temporary diagnostic probes sampled App mode, browser provider settings, Slides, Generate,
Layers, Mask and Timeline, plus a long Layer Editor title and a multi-reason blocked quote.
All nine final diagnostic probes passed; they record findings rather than asserting that the current UI is defect-free.
The probes compile only the UI test crate against the exact existing dependency fingerprints;
they do not change those dependencies. Captures cover 640 × 400, 800 × 520 and 1440 × 900 at
100%, and a physical 1280 × 720 window at 200%, in dark and light themes. Geometry and source
inspection accompany the screenshots. **Capture correction:** the stored baseline files labelled
1280 × 720 at 200% were emitted at 2560 × 1440 because the temporary fixture supplied physical
size to a logical-points API. Their geometry dumps record the actual screen in points; the
640 × 400 baseline images remain the narrow-window evidence. The repair fixture corrects the
conversion and checks physical 1280 × 720 at 200% explicitly. Underlying canvas controls and content intentionally
scrolled out of view are not counted as overlay-layout failures.

The browser settings captures render its browser-only endpoint list in a native harness.
They prove the panel's layout, not that a WASM browser session or CORS request succeeded.
Native desktop automation was unavailable during the audit; screen-reader speech, OS dialogs,
real providers and real relay connections were not manually tested. All generation uses mocks;
quoting and cancelling the probes submitted no run.

Screenshots, full logs, geometry dumps, temporary probe patch, compiler arguments and issue
packets are local review artifacts, outside the repository. Screenshots are not committed.
The findings were kept local following the owner's instruction; no GitHub issue was created.

## Follow-up findings

| Finding | Evidence and affected source | Required acceptance |
|---|---|---|
| Mask controls leave the viewport | At 640 × 400, `Brush` starts at x = −40, the prompt at x = −40, and the modal extends beyond both sides. The same occurs at 200% in 1280 × 720. [`mask_editor.rs`](../../crates/ui-egui/src/app/mask_editor.rs) fixes the body to `viewer_width`; image height reserves 60% of the window without accounting for all controls. | Bound the body to the viewport in points, keep every control reachable, preserve the image/stroke transform during resize, and test both themes and interface sizes. |
| Direct modals lack named Dialog semantics | Generate, Mask, App mode, Layers, Slides and Timeline expose no named AccessKit Dialog or modal flag in their sampled trees. [`floating::question`](../../crates/ui-egui/src/floating.rs) sets all three explicitly; 18 direct modal sites bypass it (inventory below). | Add shared named modal semantics without changing paid-run or destructive-action key handling. Assert the Dialog role, name, modal flag and control ancestry. Check actual speech/focus on a supported desktop. |
| Browser provider settings cannot scroll to Save | With the browser's one custom-endpoint field, Save servers is at y = 474.5…492.5 in a 400-point window. [`browser_providers.rs`](../../crates/ui-egui/src/app/browser_providers.rs) enables neither panel scrolling nor an inner ScrollArea; `floating::panel` defaults to no body scrolling. | Keep the header in view while every field and Save can be reached with pointer and keyboard at small sizes and 200%. Save through the command and verify the stored value. |
| Long editor titles expand the full-screen modal | A valid 108-character Layer Editor title makes its modal extend x = −38…677 in a 640-point window, with Done at x = 625.9…665.1. The non-wrapping header in [`layer_editor.rs`](../../crates/ui-egui/src/app/layer_editor.rs) places the unbounded title beside the actions. Similar title rows in Slides, Timeline and App mode require the same regression sweep; those sibling cases were source-inspected, not independently reproduced with long titles. | Constrain/truncate the title, keep it accessible in full, reserve the actions' width and test long names/notices at 640 points and 200%. Done, Undo and Redo must stay reachable. |
| Blocked-run details need bounded overflow | [`generate.rs`](../../crates/ui-egui/src/app/generate.rs) caps reasons to eight and each to 240 characters, but the modal has no scroll or height bound. Eight full-length admitted reasons make the modal extend y = −89…489 in a 400-point window, with Cancel at y = 464…482. See the local long-quote captures and geometry; the normal single-card confirmation fits. | Scroll the details while keeping Cancel reachable; preserve the full admitted explanations and never start a paid run with Enter. Test all eight full-length reasons at small sizes and 200%, including keyboard cancellation and zero submitted runs. |

Each row is a scoped `area:ui`, `difficulty:medium` follow-up to #959. They concern editor/form
content or dialog semantics rather than #1115's shell/minimap fix. The accessibility row extends
beyond #1118's individual glyph-control names. The Mask finding differs from #728's Windows
timing failure.

The native Agent sidebar also needs a focused accessibility follow-up:
[`agent_panel.rs`](../../apps/nodecraft/src/agent_panel.rs) uses a glyph-only `@` menu whose hover
text says “Mention a node” but has no accessible-name override. Its Endpoint combo is not linked
to its visible label, and its disabled Clear/Settings controls have hover text without disabled
hover explanations. Existing guard tests scan `crates/ui-egui/src`, so these app-layer call sites
escape them. This is source evidence; this audit did not run a new Agent AccessKit probe. A
small `area:agent`, `difficulty:easy` issue should name the controls, attach the Endpoint label,
retain disabled reasons and extend the native-host tests. These are follow-ups to the already
closed #1118/#1113 rather than evidence that their original code did not land.

## Complete surface inventory and coverage

“Existing tests” identifies established code coverage, not a claim that every state was retested
manually. “Probe” means new diagnostic screenshot/tree/geometry evidence in this audit. No
surface with only source inspection is represented as runtime-verified.

| Surface | How it opens | Coverage and review outcome |
|---|---|---|
| File and Open recent | File in the application row | Existing `file_dialogs::tests`, `small_windows1115_tests`, `ui_scale_tests`; shell/size tests passed. Recent entries and disabled reasons covered. Native OS picker appearance remains host-specific. |
| More | Application-row overflow | Existing `topbar_tests`, `small_windows1115_tests`; order, open-state retention, focus and Escape tests passed. |
| Quick-add | Double-click canvas or shortcut | Existing `palette1121_tests`, `palette_scroll_tests`; grouping, ranking, unavailable reasons, corner placement and scrolling tests passed. |
| Turn into | Drag a wire onto empty canvas | Existing `turn_into_tests`, `palette_scroll_tests`; inventory reviewed; no new isolated capture. |
| Search | Search or Ctrl/Command+F | Existing `menus959_tests`, `topbar_tests`, search tests; open/close and shell tests passed. |
| Discussions and comment pins | Comments or a canvas pin | Existing `comments` and `comment_layout_tests`; source inspection of pin/panel admission and empty state. No new long-thread capture. |
| Tag filter and tag row menus | Tag filter in the row | Existing tag tests; #958 redesign remains open with PR #1035. Do not duplicate its claim or treat the pending redesign as shipped. |
| Format selection | Application row or selected-card menu | Existing `layout::tests` verifies popup ownership and undo; source reviewed. |
| Card and field context menus | Secondary-click a card/header | Existing tests in `app::tests`, `fields_tests` and feature modules; representative field/keyboard tests passed. Expanded combinations still lack one complete layout sweep. |
| Input-order badges and wire actions | Select/drag a wire badge | Existing canvas/badge/keyboard-connect tests; #1114 targets and #1108 focus indicator are existing open work. |
| Generation toolbar | Select a generation card | Existing `generate_tests`; quote/model/aspect behavior reviewed, plus Generate probe. Rest/pressed painted states remain tracked by #1112. |
| History strip and result menus | Select a result/history chip | Existing `history_tests`, `copy_result_tests`, `save967_tests`; source reviewed. No new capture of every media-kind submenu. |
| Mentions in a prompt | Type a mention in a composer | Existing composer/mention tests; source reviewed. Native Agent mentions are a distinct app-layer gap above. |
| Minimap and navigation badge | Canvas navigation chrome | Existing `small_windows1115_tests`, minimap tests; small-window visibility/space tests passed. Focus remains tracked by #1108. |
| Sheets panel | Sheets | Existing `menus959_tests`, `sheet_tests`; opening geometry and close tests passed. |
| Batch sheet/table, row settings, template builder | Choose/create a sheet or use its tools | Existing `sheet_tests`, `sheet_tests::batch`, sheet tools tests; source reviewed. No new large populated-table capture at 200%. |
| Techniques, Assets, History and Styles tabs | Techniques | Existing `menus959_tests`, library browser/view tests; opening geometry passed. Populated/tab-expanded states remain individual-test coverage, not one universal layout sweep. |
| Elements library, rename/share/export forms | Elements and each entry's menu | Existing element-library tests; opening geometry passed; content/empty/confirmation patterns source-reviewed. |
| Queue and held results | Queue | Existing `held_restart_tests`, queue/run tests; source-reviewed with per-panel layout checks. No new 16-held-result narrow capture. |
| Budgets | Budgets or a blocked quote | Existing top-bar inventory and budget tests; shell geometry passed. Spent amounts and validation source-reviewed. |
| API keys and endpoint editing | API keys | Existing top-bar inventory, keys and endpoint tests; settled opening geometry passed. Native keychain prompts not exercised. |
| ComfyUI servers and hosted-GPU rows | ComfyUI | Existing inventory/state and `comfy_server::tests`; opening geometry passed. Configured/hosted rows retain their own tests; no live server used. |
| Provider servers (browser) | Providers | Existing settings tests passed; new browser-equivalent layout probe found the unscrollable body. WASM runtime remains unverified. |
| Plugins, grants and uninstall | Plugins | Existing inventory and `plugins::tests`; opening geometry passed. Expanded grant/package states source-reviewed; no untrusted package installed. |
| Automation/MCP | Automation | Existing `automation::tests` has service-backed layout checks. Source-reviewed; no live external agent connected. |
| Share live, idle/connected states | Share | Existing `sharing::tests` has layout checks for both states. Source-reviewed; no real relay session started. |
| Charges to review | `usage.ownership` in the palette | Existing `ownership_panel::tests` covers layout and decision flows. Source-reviewed; no actual charge resolved. |
| Choose a model, search and ComfyUI group | Change model… | Existing `models::tests`, `models::comfy970_tests`; representative keyboard/error/resize tests passed. |
| Workflow fields | ComfyUI card → Edit workflow fields… | Existing `comfy_mapping::tests` covers layout, 640-point opening and Escape ownership. Source-reviewed. |
| Batch items/Cross grid | Fanned card → Review items… | Existing `batch_review_tests` covers layout and Escape ownership. Source-reviewed; large-grid usability remains an explicit state to expand. |
| Custom shader and validation messages | Shader card → editor | Existing `shader_editor_tests`; source-reviewed scroll limits, error text, disabled Apply and font/parameter contracts. |
| Image viewer | Expand a result | Existing `runs_tests`, `preview_tests` cover panel layout and tall images; source-reviewed. |
| PDF reader | PDF card → Read PDF… | Existing `reader_tests`; paging/selection/close tests passed. |
| Shared parameters/inspector | Select several compatible cards | Existing `small_windows1115_tests`, bulk tests; small-window shell tests passed. Reset/popup/dirty-value behaviors have dedicated existing tests. |
| Native Agent sidebar, settings and @ menu | Agent shortcut | Existing `agent_panel::tests` includes narrow width and send/approval/settings flows. Source inspection found accessibility omissions above; no new app-host capture. |
| Native file picker / OS sheet | Open, Save as, folder/font/media pick | Existing host-seam/mocked-dialog tests; OS layout and platform speech require native manual verification. |
| Notices, stale checks and update banner | Command/background result | Source inventory, existing notice/update tests. #1110 is open with two PRs; the earliest PR remains the claim winner. No duplicate issue filed. |
| Pending drop, folder import, library import, background export progress | Drop/import/save operations | Existing per-feature tests and source review. Long filenames/errors, cancellation and placement need an explicit combined 200% sweep. |

## Direct-modal inventory

The 18 direct sites below use `egui::Modal::new` outside `floating::question`. The shared
question helper itself is a nineteenth construction site and is separately covered by named
Dialog/keyboard tests. Generated UI controls are part of their owning surface rather than
additional unlisted windows.

| Direct modal | Opens from | Review/coverage |
|---|---|---|
| App mode | Technique → Open as app | Existing behavior tests passed; new probe in both themes/sizes; semantic finding. |
| Slides editor | Deck → Edit slides… | Existing behavior tests passed; new probe; semantic finding and sibling long-title review needed. |
| Deck presenter | Present selection/deck | Existing presenter behavior tests passed; source-reviewed; no new presenter layout capture. |
| Layer Editor | Layer Editor → Edit layers… | Existing behavior tests passed; new ordinary and long-title probes; viewport and semantic findings. |
| Timeline editor | Timeline → Edit timeline… | Existing behavior tests passed apart from the isolated export timing noted above; new probe; semantic finding. |
| Mask | Image tools → Inpaint… | Existing behavior tests passed; new probe; viewport and semantic findings. |
| Generate/run quote | Generate, Run changed, Run workflow | Mock-backed probe; normal and multi-reason blocked quotes; semantic and bounded-detail findings. |
| Tag definition | Rename/delete tag | Existing pointer/keyboard/undo tests passed; source-reviewed; use named question semantics in follow-up. |
| Native recovery | Restart after unclean exit | Existing recovery tests, source-reviewed; persistence proof is tracked separately in #1120. |
| Browser recovery | Browser journal found | Existing `web_journal::tests`; source-reviewed only in this audit. |
| Startup keep-aside | Slow startup journal preservation | Existing `keep_aside::tests`; source-reviewed; runtime proof belongs to #1120. |
| Native unsaved changes | Open while modified | Existing file-dialog tests; source-reviewed; no new OS-session capture. |
| File progress | Save/open/recover after its delay | Existing progress/cancel tests; source-reviewed; no new long-path capture. |
| Workflow result save | Save workflow results… | Existing export tests; source-reviewed preview/working/done states; add viewport/speech checks. |
| Export close | Quit while export finishes | Existing close/cancel tests; source-reviewed; preserve its deliberate operation-ownership behavior. |
| Card export progress | Export-to-folder card | Existing `card_export746_tests`; source-reviewed; no actual private destination used. |
| Replace existing files | Card export configured to overwrite | Existing deliberate replacement tests; source-reviewed; keep Enter from accidental replacement. |
| Full text preview | Expand text action output | Existing text-preview tests; source-reviewed; add named dialog check. |

Other question surfaces already use `floating::question`: Rename field, Review model change,
Model preview changed and browser Unsaved changes. Their existing representative keyboard,
focus, Enter/Escape and small-window tests passed. Those successes do not cover the 18 direct
modal callers above.

## Tracker reconciliation

At the reviewed snapshot, #1125's Markdown still shows every child unchecked, but GitHub reports
**12 closed and 5 open**. Closed: #1106, #1107, #1109, #1111, #1113, #1115, #1116, #1117, #1118,
#1121, #1123 and #1124. Open: #1108 (focus), #1110 (notices), #1112 (control states), #1114
(target sizes) and #1119 (first-run guidance). The associated open PRs are evidence of work in
progress, not shipped fixes. #958 is also still open and remains a dependency for the final
per-surface #959 close-out. No tracker or claim was changed during this local-only review.

## Remaining close-out work

Fix and regression-test the findings above, run the actual browser and native screen-reader
checks, and extend coverage from opening states to populated, expanded, failure and long-content
states. In particular, the current overlap helper cannot distinguish scrolled-away content from
clipped content because egui's accessibility tree does not identify all scroll viewports. Keep
that limit explicit rather than calling every raw offscreen accessibility rect a defect.

Before closing #959, attach the required per-surface before/after media, verify keyboard focus
and Escape ownership for all remaining panels, and reconcile its acceptance checklist with the
inventory. This audit supplies evidence and a complete surface index; it is not a claim that the
remaining checks or fixes passed.
