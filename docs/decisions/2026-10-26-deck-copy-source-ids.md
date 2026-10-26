## 2026-10-26 · Rebase copied Deck references through one engine helper (#695)

- Supported Deck source layers follow only nodes present in the same explicit old-to-new ID
  map. The engine helper is shared by UI clipboard placement, `canvas.paste` copy mode and the
  Technique library's canvas-copy path. Copying a Deck alone preserves external references.
  Unknown layers, unsupported Deck versions and unreadable stored data travel untouched.
- Legacy `canvas.paste` callers supply final node and wire IDs. Those payloads contain no copy
  provenance, so the engine cannot infer which original ID a final ID replaces. The optional
  `node_ids` map instead lets CLI/MCP callers send original node IDs and wire endpoints; it
  must cover every pasted node with a unique fresh target. The command rebases those IDs and
  supported embedded references once, before publishing one ordinary invertible changeset.
  Wire IDs remain caller supplied. Omitting the map preserves the existing final-ID contract.
- The UI already builds a complete fresh-ID map while placing its clipboard payload. It calls
  the shared helper and uses legacy final-ID command mode, avoiding a second remap.
- Current published-Technique validation requires every node to feed an exposed output. Decks
  are annotations with no execution ports, so a valid Technique currently cannot contain one.
  Keep that invariant; the library copy path uses the common helper defensively if supported
  annotation rules change later. Do not manufacture malformed Techniques to test placement.
