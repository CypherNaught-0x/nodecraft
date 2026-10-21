## 2026-10-21 · Timeline interchange is FCPXML 1.10 in a ZIP with its media (#166)

- **FCPXML 1.10 is the first target.** CMX3600 EDL can't layer past one picture track and
  matches media by reel name. FCP7 XML usually points at absolute `file://` paths. FCPXML
  takes `media-rep src` as a URL, so a relative path should keep the archive portable;
  whether each editor resolves it against the document is unverified (#643).
  The evaluation and the feature matrix are in `docs/timeline-interchange.md`.
- **The writer is pure and lives in `nodes`** (`timeline::interchange`). It takes the clips, the
  wires and a list of sources and returns the XML, the losses and the sources it used, so it is
  testable without a session or a store. The archive writer lives in `format`
  (`interchange::Bundle`), beside the asset store it reads. The engine only joins the two.
- **Everything lands on whole frames.** A cross transition becomes a dissolve centred on the cut,
  with half of it from each clip, so the sequence length matches the render. Off-frame times are
  rounded and reported.
- **Losses are data.** Each setting the document can't carry is returned as a `Loss`, shown by
  `timeline.interchange` and written to `report.txt`. Nothing is dropped unseen. Fades and a
  non-black background are the current losses.
- **Missing media is an error, not a hole.** A wired node with no result stops the export,
  naming the clip, rather than writing an offline clip.
- **`history.export`'s file path is shared.** `Effect::Export` carries an `Exported`, either an
  asset or a bundle, so both use the same staged, atomic write and path rules and the same error
  messages, which never repeat the path. The export is synchronous for now; a cancellable
  background export is #643.
- **`timeline.export_interchange` is human-only on MCP**, like `history.export`, because it writes
  a file on this machine. The read-only preview is open to agents.
