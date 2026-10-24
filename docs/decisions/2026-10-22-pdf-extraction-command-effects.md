## 2026-10-22 · PDF extraction is a typed media effect followed by one edit (#606)

- **One command path.** `document.extract_page` and `document.extract_text` serve the reader,
  CLI control, MCP and agents. Their JSON pages start at one, like `document.use_pages`.
  Existing Rust extraction adapters retain their zero-based page indexes and dispatch these
  commands. The document event records the extraction command, rather than a nested `node.add`
  or `canvas.paste`.
- **Availability does not read a PDF.** A handler checks parameters, project metadata and edit
  permission, then describes an opaque `DocumentExtract` effect. Only dispatch verifies and
  reads the source. Availability cannot predict corrupt page contents or a reader failure.
  Viewer and commenter roles are refused before reading or decoding; extraction needs edit
  permission even though discussions alone are changes too.
- **Prepare, then publish.** Session stages a copied PDF through its existing opaque import
  receipt and prepares an AddNode changeset against that exact candidate asset store. Text
  uses the same ordinary changeset staging without an import. Publication, persistence, event
  and undo follow the existing prepared-edit boundary; a failure never publishes a partial
  asset map or an extra undo step. Sources remain unchanged.
- **Bounded, local work.** Source and copied PDF convenience bytes stay within 16 MiB and the
  reader's existing limits. A source descriptor over that limit refuses before verification
  could stream its contents. Extracted text stays within 960 KiB at a character boundary.
  Regions are finite, normalized rectangles with positive area. Omitted IDs are generated
  only at dispatch with a fallible random source and bounded collision retries; placement
  defaults beside the source within the existing canvas bounds. No dialog, provider or OCR
  capability is required, including in the browser's pure Rust PDF path.
