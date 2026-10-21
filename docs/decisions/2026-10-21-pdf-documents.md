## 2026-10-21 · PDFs become document nodes read by two actions (#113)

- **One reader, pure Rust.** PDFs are read by `craft-pdf` (craft-libs' patched Hayro), behind
  `nodecraft-media`'s `pdf` feature, which builds for wasm. Its Hayro crates come from craft-libs
  through root `[patch.crates-io]` entries; drop them when the fixes are released upstream. The
  substitute fonts, CJK character maps and CMYK profile it embeds are in `ATTRIBUTION.md`.
- **The source is never changed.** A dropped, pasted or imported PDF is a `source.document`
  node; its card shows the first page and its page count (`meta.page_count`, probed at import).
- **Reading is actions, not commands.** PDF to images (`document.pages`) and PDF to text
  (`document.text`) are deterministic actions with a page range (`first`, `last`, where 0 means
  the end), so they recompute, chain and run headless like any other action.
- **Bounded by input.** At most 64 pages drawn and 2,000 pages read per run; a wider range is an
  error asking to narrow it. Pages are drawn at most 4096 px on the longer side; files above
  256 MiB, and text above 1 MiB a page or 4 MiB a run (cut at a character boundary), are refused
  or cut.
- **Text is what the pages draw**, in content order. Spaces and line breaks are inferred from
  glyph positions; a scanned page has no text, since there is no OCR. OCR, picking pages from
  thumbnails and splitting a PDF into new PDFs are follow-ups.
- **Test PDFs are built in code** by `nodecraft_media::pdf_fixture` (the `fixtures` feature,
  dev-only), so no binary fixture is committed.
