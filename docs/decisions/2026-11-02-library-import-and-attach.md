## 2026-11-02 · Library entries are ticked, placed together, exported and imported through existing paths (#96)

- **Placing several entries is one command, `library.attach`.** It takes up to 32 entries, each
  an asset the project holds (`{hash}`) or one value of a past result (`{from: {canvas, node,
  generation, port?, item?}}`, the address `history.extract` and `library.history`'s `place`
  use), and adds one source card per entry in a grid, as one undo step. It makes exactly the card
  `history.extract` or the Assets browser's Insert would, so it adds no new kind of edit. The
  caller names only *which* entry: an asset's type and metadata come from the document's own
  reference to it, and an asset whose bytes the store no longer holds is refused. Any refused
  entry refuses the whole request, so the step is all or nothing.
- **Provenance is the content hash plus a title.** The project has no per-card provenance field,
  and adding one would change the file format for a label. A placed card holds the same
  content-addressed asset (the same hash, so `library.assets` counts it as another use of the
  same bytes), is titled after the card and result it came from ("Painter · 2/5", or the source
  card's own title), and the command's result reports each card's `from`. Generation ids never
  change, so `from` stays valid while that result exists.
- **Export uses the workflow-result save, not `export.local_run`.** `export.local_plan` and
  `export.local_run` write each source's *active* result by design (#98), while a ticked History
  entry is usually an older one. The workflow-result save (#966) takes explicit `targets`
  (node, generation, port, item) and never changes which result is active, over the same
  `ExportHost` folder grant: a person picks the folder in the shell, nothing is written before
  they press Save files, and programmatic callers get no dialog (they dispatch
  `export.results_plan`/`export.results_start` with a host grant). The browser build and
  non-Unix desktops get the existing refusal naming Save as as the alternative. An asset that is
  a card's own parameter (an imported file) is not a result, so it doesn't export from the
  browser; one made by a card on the open canvas does.
- **Import is a host operation, like a drop.** A command can't carry media (a command's strings
  share a 1 MiB budget) and a path in a parameter is never authority to read a file, so the
  bytes come from the host: the desktop app reads files a person picked on its own thread and
  hands them to `nodecraft_engine::import::import_library`, which publishes through the drop's
  bounded, single-transaction `canvas.paste` import (#197). That gives the library the drop's
  sniffing (bytes decide the type, never the name), its per-file and per-project byte caps and
  its candidate-store transaction for free. What the library adds is one outcome per file in
  input order (added, duplicate, already in the library, failed with why), deduplication by
  BLAKE3 before anything is stored (a duplicate or an asset the project already lists adds no
  card), media only (text and ComfyUI workflows are still dropped on the canvas), and the file's
  name, never its folder, as the card's title. The reader is bounded like a folder import's:
  regular files only, 16 MiB each, 48 MiB and 32 files per import, counted for progress and
  cancellable between files. The failed files' paths stay in the window's view state for Retry
  failed; they never reach the document, a log or a command.
- **No URL import yet.** The desktop app has no general network platform service with a host
  network policy: provider adapters reach only their own hosts, and the one public-address check
  (`nodecraft-cli serve`'s webhooks) lives in the CLI. A URL import needs that service first,
  with an owner decision on its policy (https only, which hosts, resolving and refusing private
  and link-local addresses on every redirect, byte caps, content-type checks, no credentials),
  and an off-thread fetch that hands bytes to `import_library` like the file reader does.
- **No cross-project global library yet.** The Elements library (`2026-10-22-elements-library.md`)
  shows the shape one could take: a versioned file plus a content-addressed media folder in the
  settings folder, locked against a second window, copied into a project on placing. An asset
  library differs in ways that need an owner decision first: it holds arbitrary media up to the
  project limits rather than a few reference images, so it needs storage bounds and eviction
  (#147), a rule for what is shared from private canvases, and garbage collection across
  windows. Until then the browsers list the open project only.
