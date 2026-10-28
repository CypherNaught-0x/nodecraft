## 2026-10-27 · PDF cooperative checkpoint dependency (#596)

Pin `craft-pdf` and its seven patched reader/writer backends together to immutable revision
`aaa46f2e524c209efb0f21bd387974ee58a28dd9`. It provides synchronous interpretation
checkpoints for cancellation and a caller-owned work budget, including single-threaded
WebAssembly. It also refuses cancelled raster output instead of returning partial pixels.
The application integration follows in a separate change.

The change is published in [craft-libs PR #240](https://github.com/storytold/craft-libs/pull/240),
updated against upstream `9ff719499d8f4730b93071abed993e5f12fe1be8`. Its original independent
review and its integration verification are clean. Upstream merge remains with its maintainers;
this account has read-only access there. Keep this dependency update separate and record the
immutable source even if the upstream PR later receives a merge commit.

Keep `craft-ui`, `craft-settings`, `craft-atomic` and `craft-quit` at their existing revision.
They share NodeCraft's egui 0.36 interface; newer upstream UI crates use egui 0.37. The PDF
facade exposes media data and does not require that UI migration. No backend source is copied
into NodeCraft. Existing native and browser contracts must pass with the new dependency.

The CMap and PostScript scanners are part of that backend graph too. Their root patches
retain the iterative delimiter parser; the registry parser can overflow its stack on a long
run of dictionary delimiters. A real PDF ToUnicode fixture verifies that the final mapping
is read. The regression aborts with a stack overflow against the registry parser when built
without optimization, and must pass with the pinned parser. Default fixtures keep their
original bytes. The final upstream revision adds only corrected consumer-pin documentation
after the reviewed code at `59587f0b437bd0aae7f64b3696ebe38f2954b394`.
