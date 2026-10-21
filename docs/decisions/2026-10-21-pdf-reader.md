## 2026-10-21 · The PDF reader: a view over a PDF that edits through commands (#590)

- **The reader is view state.** Right-click a PDF card and choose "Read PDF…", or "Choose pages…"
  on a PDF to images or PDF to text node, to open a window with the page, a thumbnail strip, zoom
  and paging (buttons, arrow keys, Page Up/Down, Home/End). The page, zoom, scroll and selection
  rectangle stay in the shell and never reach the document, so reading a PDF is never an edit.
- **Choosing pages is a command.** In page-range mode, "Only this page", "Start here" and "End
  here" dispatch `document.use_pages {canvas, node, first, last}`, which sets both parameters in
  one changeset (one undo step). It is an ordinary command, so the CLI, MCP and the agent can set
  a range the same way; the node's results go stale like any parameter edit.
- **Extractions are session functions.** "Extract page" copies one page into a PDF of its own and
  adds it as a new PDF node; "New Text node from page/selection" puts the text the page (or the
  dragged rectangle) draws into a new Text node. Both store or read project media, which a command
  can't, so they follow `split_grid` and `import_files`: session functions, each one undo step.
  Exposing them to the CLI and MCP is a follow-up.
- **The page is copied, not re-rendered.** Extraction embeds the page with Krilla's PDF embedding
  (via `craft-pdf`'s `write-pdf` feature), so text stays text and vectors stay vectors. Region
  text is the text whose glyphs fall inside the rectangle; a scan has no text and there is no OCR.
- **Bounded.** Extracted text is cut at a character boundary to 960 KiB, within a command's
  string budget. Page previews are cached per page at 256, 1024 or 2048 pixels on the long edge.
