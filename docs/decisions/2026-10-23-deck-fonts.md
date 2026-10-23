## 2026-10-23 · Decks keep their own font references (#693)

- **Use the card's existing font collection.** Decks store `[name, asset]` pairs in the same
  node-config field as [Layer Editor fonts](2026-10-22-layer-fonts.md). The slide JSON stays at
  version 1. Copying, saving and undo carry those references with the card, and content-addressed
  storage retains one copy of bytes shared by several cards. The shared command helpers retain
  at most sixteen valid font entries, each at most sixteen MiB; damaged metadata is skipped
  before copying the bounded collection. Lato remains reserved. Names come from the file's
  declared family/style and compare without case or surrounding spaces.
- **Edit through revision-checked commands.** `deck.font_add` takes an imported TrueType or
  OpenType asset; `deck.font_remove` takes its name. Both check the deck version, edit permission
  and optional `expected_revision` before reading or changing fonts. Each is one changeset and
  one undo step. `deck.list` returns the held references and each text layer's requested name,
  resolved name/reference and whether an unknown name falls back to Lato. This resolves names
  without re-reading whole font files on every listing; drawing additionally checks that the
  referenced bytes can be read and parsed.
- **The same fonts draw everywhere.** The slide editor, its thumbnails and the presenter use
  the same layer renderer and cached font payloads. A deck shares its validated `Arc` payloads
  across slides and the properties panel. Texture keys include names, asset hashes and
  availability. A missing or unreadable font draws as Lato, preserves the requested name, and
  shows a visible notice in the editor or presenter. A successful explicit re-import retries
  missing cache entries and redraws the fallback even when the graph itself is unchanged.
  Resident and indexed font sizes are checked before copying or reading their payloads.
- **An asynchronous picker cannot overtake an edit.** The desktop reuses the existing bounded
  file picker/worker. A pending deck pick captures its starting revision; changes while the
  dialog/read is pending refuse its result. A frame's edit or undo takes precedence over a ready
  file, and closing or switching the slide drops the pending pick. Importing the font and
  assigning it to the chosen text layer remain two undo steps. Only a validated successful
  import advances the revision used for that assignment. The browser has the same commands and
  rendering; as for the Layer Editor, its file-picker UI is not yet offered.
- **Prove persistence and the visible behavior.** Normal tests use the existing bundled OFL
  font with a test-only family name, exercise per-card scope and `.ncraft` round trips, and drive
  real font-choice/undo/presenter controls in both themes. Tests also cover pending-read undo
  and close, unknown names and re-import without a revision change. Opt-in actual GPU captures
  use a locally installed font by environment path; that font file is never committed.
