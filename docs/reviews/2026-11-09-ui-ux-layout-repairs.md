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
notice, keeps every header action in view, and verifies that view changes do not edit the
project. The blocked-quote test scrolls to its final reason, checks that Cancel does not move,
presses Enter without submitting a run, and then cancels with a pointer click.

Optional public-safe repair screenshots are emitted by these tests when
`NODECRAFT_LAYOUT959_EVIDENCE` names an output directory. The baseline screenshots and local
issue packets remain outside the repository. Full integration validation and independent review
are recorded with the completed local integration; this record does not replace those gates.

#959 still includes broader coverage work identified in the baseline inventory. These four
repairs do not by themselves close its entire per-surface acceptance checklist.
