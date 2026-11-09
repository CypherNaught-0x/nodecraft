# UI inventory acceptance follow-through (#959)

This follows the [baseline audit](2026-11-09-ui-ux-audit.md). It preserves that report's original
findings rather than replacing its before evidence with repaired UI. The seven findings now
have local repairs and focused regressions. The integration task owns the final combined gate
and the local before/after capture packet; this report does not claim a GitHub issue was closed
or a PR was published.

## Finite acceptance and completed test gaps

The original acceptance requires a complete inventory, actual open/key-control/close tests for
each menu/panel, shared overlap checks at agreed sizes, documented outcomes and before/after
media. It does not require every possible populated/error state, paid provider calls, real relay
accounts, OS-owned picker appearance or actual screen-reader speech. Those remain useful
subsequent checks. Browser-owned app questions can be tested against the existing fake host.

| Previously missing proof | Actual regression now present |
|---|---|
| Generic opening controls beyond a shared header | `menus959_tests` checks Sheets' New sheet/Title; Techniques' four tabs; Elements' Import; Budgets' cap labels/Save; API keys' empty disabled Save; ComfyUI's empty guidance/Add; Plugins' unavailable guidance. Original toggle-close assertions remain. Both themes, 1000×700 and 1600×900. |
| Seven floating tools lacked standard dismiss/closed-state assertions | Queue, Share, Automation, Charges, Shader, PDF reader and Image viewer now exercise real HeaderClose and Escape in their existing feature fixtures, with the caller's actual closed state plus absence from `floating::shown`. Before dismissal the shared helper checks the current opening state at 1000×700 and 1600×900, then restores the feature viewport. |
| Dismissal could accidentally change service/data state | Queue Escape preserves held result bytes; Automation Escape leaves its server disabled; PDF paging/close preserves the editable document; Charges still produces the original Preview/Confirm receipt; all original feature assertions remain. |
| Browser question coverage answered state directly | `web_journal::tests::recovery959_question_buttons_restore_or_discard_and_then_close` clicks Recover/Discard, verifies recovered project/deleted slot, input ownership and button absence. `web_files::tests::files959_unsaved_question_cancel_closes_and_preserves_the_project` inspects Discard/Cancel and clicks Cancel, preserving document and clearing pending phase/buttons. Both themes at 640×400; existing deliberate Enter/Escape semantics remain. |
| Folder import cancel skipped the actual control | `folder_import::tests::reading_can_be_cancelled_and_nothing_is_added` holds a worker channel, inspects real progress, clicks Cancel, verifies cancellation signal, progress/control disappearance and unchanged document. |
| Field/history actions lacked explicit popup absence | Existing field Collapse/Move-into and history Delete/Extract flows retain their behavior assertions and now assert the popup action is absent after choosing it. |
| Search/Comments inventory covered one theme | Existing actual open/control/toggle-close flow now runs in both themes and both agreed sizes. |

Generic frontmost tool Escape and its text/gesture/question/notice guards are covered in
`tool_escape959_tests`, independently of these feature lifecycle regressions. The native Agent
sidebar already had a full actual shortcut-open/input/send/focused-Escape-cancel/shortcut-close
flow in the app crate. Escape there deliberately cancels work rather than hiding the sidebar.
The shared-parameters small-window fixture explicitly checks its incompatible-port explanation;
normal mixed editing controls are covered by `bulk71_mixed_text_edits_all_with_one_undo_and_owns_shortcuts`.

## Reproducible evidence

The same feature tests can optionally render their public opening state immediately before
actual dismissal when `NODECRAFT_MENU959_EVIDENCE` names a local directory. Additional hooks
in established field/history/mention/model/batch/shared-parameter/timeline/import fixtures use
the same variable. No screenshot is committed, no live provider is called, and idle Share
captures omit ephemeral room credentials.

The local packet records the source revision, exact dependency fingerprints, compiler command,
test results, PNG dimensions and SHA256 hashes. It also maps each inventory row from the
baseline audit to its specific existing/new test and its current capture status. Matched captures
from unchanged UI are still actual before/after captures; an after-only image is not relabelled
as baseline. Thirty established hooks already produced 80 actual matched PNG pairs; the old
File workflow hook is explicitly partial because its Open-recent helper fails on both versions
after capturing the valid Unsaved question. A separate native default gallery uses isolated
profiles and existing self-contained desktop executables.

The audit's old files labelled 1280×720 at 200% actually rendered 2560×1440 because its temporary
fixture passed physical size to a logical-points API. Their original geometry/provenance remain;
valid 640×400 before images are retained. The layout repair fixtures instead assert exact
physical 1280×720 at 200%. Long-notice repair captures are exact physical 640×400 in both themes
at 100% and 200%. Underlying canvas controls and intentionally scrolled-away content are not
counted as panel overlap defects.

## Finite evidence completion and navigation follow-through

The local inventory packet now contains 153 actual matched PNG file pairs: 108 established
fixture pairs, 23 native gallery pairs and 22 additional fixture pairs. These are file pairs,
not 153 unique surfaces. Of those, 152 share a viewport; the legacy Tag-filter Undo comparison
is explicitly a semantic state pair with different sizes, with separate same-size #958 footer
evidence retained. The fresh baseline fixture captures use exact `caecbb32` product source and a test-only
capture patch. Browser question fixtures apply the selected theme to both app tokens and egui;
earlier mixed-theme Light diagnostics are preserved as invalid evidence, not counted as the
final Light-theme pictures. Source, patch, image hashes and actual fixture results remain in
the local packet. This completes the concrete original capture gaps.

That completed sweep exposed an eighth preexisting finding: fixed navigation painted over
floating-tool controls. A real pointer in a broad Batch grid cell executed Fit all instead of
selecting the item; a covered Shader request field could not receive text. Navigation now keeps
its geometry above the canvas root and below floating tools. Its early minimap admission checks
the actual painted layer outside the Context input lock. The shared layer identity preserves
ordinary keyboard quick-add over an uncovered idle map.

Both pointer regressions pass in both themes, including last-row scroll/select and unchanged
view/document/no-provider invariants. An additional quick-add/undo regression passes. The
focused correction passed 151 test executions across 13 filters (including repeated tests);
initial independent review and sole verification closed without unresolved findings. Separate
Dark before/after repair pairs and additional Light after captures preserve this repair without
relabeling the 153 historical inventory pairs. The parent integration task records the final
full Cargo gate result separately.
