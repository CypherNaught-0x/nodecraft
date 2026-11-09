# UI layout repairs — 9 November 2026 (#959)

These repairs address four layout defects reproduced in the
[baseline audit](2026-11-09-ui-ux-audit.md). That audit and its screenshots describe the earlier
snapshot; this record describes the resulting implementation. Named modal semantics and the
native Agent accessibility follow-up are separate work.

- **Mask:** the modal's width and scroll viewport are bounded by the current window in interface
  points. The prompt uses the admitted width. The image and all controls remain reachable, while
  resizing and scrolling use the existing image-to-screen transform and never rewrite stored
  strokes.
- **Provider servers:** its body scrolls below the shared floating-panel header, allowing the
  final endpoint fields and Save servers to be reached in small windows and at 200% interface
  size. Settings still save only through the existing command.
- **Layer Editor:** Done, Redo and Undo take their space first. The title uses the remaining
  width and truncates visually; its full value remains accessible and available on hover.
  Diagnostic text uses a separate bounded row, so it cannot push actions out of the window.
- **Run confirmation:** the admitted explanation text scrolls between the heading and a separate
  action row. Cancel remains visible even with eight full-length blocking reasons. No new
  keyboard action starts a run; the existing quote acceptance and price binding are unchanged.

## Regression coverage

The new `layout959_…` kittests exercise real pointer scrolling and clicks, both themes, physical
640 × 400 and 800 × 520 windows, and a 1280 × 720 window at 200% interface size. Shared setup
applies interface scale before resizing and checks the resulting size in points.

The Mask test checks stored strokes across resize, paints again in image coordinates after
scrolling, and closes through the reached footer. The provider test scrolls, saves and reloads
settings through the origin store. The Layer Editor test checks the full accessible title and
notice, keeps every header action in view, and verifies that view changes preserve editable node parameters, title and position (the compositor may still
record derived history while the UI settles). The blocked-quote test scrolls to its final reason, checks that Cancel does not move,
presses Enter without submitting a run, and then cancels with a pointer click.

Optional public-safe repair screenshots are emitted by these tests when
`NODECRAFT_LAYOUT959_EVIDENCE` names an output directory. The baseline screenshots and local
issue packets remain outside the repository. Full integration validation and independent review
are recorded with the completed local integration; this record does not replace those gates.

#959 still includes broader coverage work identified in the baseline inventory. These four
repairs do not by themselves close its entire per-surface acceptance checklist.

## Focused validation outcome

The four `layout959_…` regressions passed on source `f0a395ff`, covering 24 theme/viewport
combinations. The UI test crate was compiled directly against the exact coherent native
artifacts from the integrated `017ed394` source; the source hash, dependency fingerprints,
compiler arguments and output log accompany the local repair evidence. No primary-cache
artifacts were modified by that proof. The 24 emitted PNGs were checked for size, and the
640-point dark and 200% light results were inspected.

Two test-fixture assumptions were corrected before this passing run: `Harness::set_size` takes
logical points after zoom, and the compositor may legitimately advance a session revision while
recording derived history. The final tests check the requested viewport and editable node data.
The product implementation did not change after the initial independent review of
`073c51d5..acddfbf8`, which found no correctness issue. Full integration gates still apply.

The long-diagnostic fixture also exposed a separate global notice overflow (#1110): its wrapped
body could extend above the viewport and hide Dismiss. That seventh local finding is preserved
with evidence and is being corrected separately; it is not hidden by the successful editor-header
checks. The baseline audit remains historical evidence, with its capture-size correction noted.
