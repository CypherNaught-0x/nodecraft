## 2026-10-21 · A batch export writes a folder with a naming pattern (#98)

This record covers **only** what [`2026-10-21-result-export.md`](2026-10-21-result-export.md)
(#557) does not already decide. #557 owns exporting *one* result to *one* path a person chose, and
this builds on its rules rather than restating or changing them: the path is absolute, the folder
must already exist, nothing is replaced without consent, and the asset is hash-verified and
streamed through the store's verifying copy. `history.export` is MCP human-only, the browser build
reports unsupported, text results are refused, and messages never repeat the path. A folder grant
needs stronger path safety than a single user-picked file: `export.local_run` uses a pinned root
and descriptor-relative no-follow writes, sharing #557's verified asset-copy primitive rather
than its path-based writer. Non-Unix platforms without those primitives refuse batch export.

What follows is what only a *batch* needs.

- **A batch is still commands, not a node kind.** `export.local_plan` names what would be written
  and writes nothing; `export.local_run` writes it. A node's `plan` produces a `JobSpec` whose
  results the run planner stores *in* the project — there is no side-effecting job type, and
  inventing one (a job that writes outside the project, replays on undo, and survives autosave) is a
  run-planner change far larger than this issue. Export also changes nothing on the canvas, so
  treating it as a graph edit would give it an undo step that cannot be inverted. This is the same
  reason #557 made `history.export` a command; it applies unchanged to the batch.
- **A destination in a parameter is never authority to write there.** #557 needs no such rule: a
  person picked the exact path in a save dialog, and that pick *is* the consent. A batch is the
  opposite shape — the caller supplies a folder and a pattern fills in the file names — so
  `export.local_run` trusts only an injected `local_export::ExportHost`: each app-authorized
  folder is resolved and opened once, with the directory descriptor kept for the grant. Descendant
  components are refused if symlinked and opened with no-follow under that descriptor; the parent
  inode is checked again before publication. Without a host both commands return Unsupported.
  `authorized/../elsewhere` and relative paths are refused. This is why `file.*` is human-only,
  and why `export.local_run` joins it there.
- **A batch is stricter than one file about when it may run.** `history.export` writes the one
  result a person just pointed at, so the session lets it proceed while a project open is in
  flight. A batch writes many files, so `Effect::LocalExport` is deliberately kept out of that
  bypass: it settles a pending save first and refuses to run while an open is in flight, rather
  than reading a store that is being replaced.
- **Which result a source means is #557's rule, not a second rule.** `(node, port, item)` resolves
  through the same `history_cmds::result_value` that `history.export`, `history.extract` and
  `history.copy` share. So a list output with no item is ambiguous and refused, an item on a single
  value is a mistake, a result whose port layout changed is a conflict, and the *active* generation
  is the payload — a batch never exports "all history". Choosing another result with
  `history.select` and exporting again is how a person exports a different take.
- **Text is refused, here too, for #557's reason.** Copying is the way out for text. A batch that
  quietly wrote `.txt` would not be extending that decision, it would be contradicting it, so
  `export.local_run` refuses a text result per entry and repeats the same advice.
- **A naming pattern is the point of the batch.** `{index}`, `{node}`, `{generation}`, `{kind}`,
  `{title}`, `{output}`, `{item}`, `{hash}` render into one path component. Literal template
  separators and `..` are refused, as are those sequences in a node title, kind or output name.
  Other interpolated document values pass through one `keep` rule: control characters and anything
  not alphanumeric, `.`, `_`,
  `-` or space become `_`; edges are trimmed; a Windows-reserved name is prefixed; the stem is
  truncated on a character boundary. An unknown token is an error, because silently dropping it
  would quietly collide every file. The extension comes from the stored MIME type and is never
  invented for a type without a usable one.
- **A collision policy only makes sense across many files.** `on_collision` defaults to `skip`, so
  an export never replaces a file the person did not choose to replace and *reports* what it left
  alone; `overwrite` stages beside the destination then replaces it atomically; `numbered` finds
  `name (2)`. Lookup is advisory: new files use exclusive `O_EXCL` creation relative to the pinned
  parent, so another process winning the same name gets a conflict, not an overwrite. Existing
  symlinks, including symlinked directory components, are refused. Phase 1 never creates the folder.
- **Exact completeness limit (including the no-hard-link fallback).** Overwrite stages before
  renaming, so a failed copy cannot partially replace an existing file; a crash or failed cleanup
  can still leave a private staging file. `skip` and `numbered` use
  direct exclusive creation so they remain safe even where hard links are unavailable, but the
  new final name can be visible *while* the verified copy runs. A crash can leave a partial new
  file; normal copy failures attempt to remove only the inode they created, but cleanup itself can
  fail. This intentionally weakens the former unconditional complete-file/no-leftovers promise.
  The pinned shared single-file `craft_atomic::write_new_with` also has a non-atomic create-and-copy
  fallback when hard links fail; it must not be cited as proof of unconditional atomic new files.
- **Cancellation stops the run between files, never inside one.** A batch checks the token before
  each file: a run already mid-file finishes or fails that file. Its outcome reports one state per
  entry rather than inventing intermediate stages.
- **One entry failing is that entry's failure.** The result carries exactly one entry per request,
  in request order — its node, generation, port, item, the name it used, its state, its byte count
  and a reason — so `partial` is never a guess. A reason is either a fixed string or a message from
  the shared resolver, verifying copy or descriptor writer, all fixed text; none carries a caller's
  path, MIME type or node title, which is #557's rule kept at batch scale.
- **Pass-through is the only output operation, and the build says so.** `FORMAT_CAPABILITY` is
  returned with every preview. `format.mode` accepts `convert` and `vectorise` *so a generated form
  can name what is unavailable*; choosing either is refused before a byte is written, with the
  requested type and the alternative in the message. Passing an SVG source through unchanged is a
  supported operation; rasterising to SVG is not implied by it and is refused.
- **`export.local_plan` is agent-visible; `export.local_run` is human-only.** A preview reads the
  document and validates the folder grant but writes nothing. Writing files outside the project is
  a person's decision, as #557 already decided for one file.
- **The apps still have to offer it.** `apps/nodecraft` must pick a folder (through `rfd`, which
  only `ui-egui` may use), inject the host, and wire a Cancel to `host.cancel()`. Until it does,
  both commands return the reason rather than writing anywhere, which is why the engine contract
  could land first. The `media.export` node kind and app-hosted execution remain follow-up #746;
  this phase delivers the engine command boundary, not a generated inspector or an in-canvas node.
