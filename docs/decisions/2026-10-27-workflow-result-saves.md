## 2026-10-27 · Workflow result folder saves (#966)

File → Save workflow results asks for one folder, then previews filenames before the person
starts writing. Selected nodes define the scope; no selection means every node with media
history on the current canvas. All history is the default; active-only is an explicit option.
The separate `export.local_*` contract remains active-result-only. The `media.export` node and
its run-engine side effects remain separate work in #746.

`export.results_plan` and `export.results_start` share typed parameters across all command
surfaces. Exact `targets` identify node/generation/port/list item without changing active
history, allowing individual result menus to reuse the folder chooser. Programmatic commands
never open dialogs. A destination string is never permission: only a trusted host injects an
ephemeral `ExportHost`. `local_export::dispatch_with_host` restores any previous grant after
dispatch; an admitted worker retains its immutable grant and selection snapshot.

The existing descriptor-relative, no-follow folder authority is preserved. Secure folder
writes currently work on Unix (macOS/Linux); Windows and the browser refuse before writing
and the UI explains that individual result exports remain available. Windows needs an
equivalent audited descriptor-relative implementation before this feature can be enabled.
The new writer stages within the chosen pinned directory and atomically links complete files
without replacement. Filesystems without hard links refuse that file rather than publishing
partial output. Collisions, including symlinks and the manifest name, receive numbered suffixes.
Normal errors, cancellation and unwinding clean staging links; an OS crash or cleanup failure
can leave a hidden `.nodecraft-results-*` file in the chosen folder. Cleanup warnings are
reported with the actual successfully published filename.

The original stored media type supplies the extension; bytes pass through without conversion.
The JSON manifest records actual relative filenames, content hashes, node/generation/output
identities and original `Generation.settings` (including submitted prompts and parameters).
Today's edited parameters are never substituted. Old runs without settings explicitly say
`not_recorded`. Credential-like provenance keys/roles and the chosen destination spelling are
redacted. The manifest has no destination field or serialized filesystem capability.

Plans are bounded to 256 media files, 65,536 visited history values, eight preview names and a
4 MiB manifest. Provenance is serialized through a bounded writer before JSON materialization.
Admission reads descriptors, never indexed media bodies. Verification/copy uses the existing
quota-accounted background export leases; large bodies are streamed outside the UI and do not
hold the live asset context operation gate.

A batch uses the same process-wide export slot, runner, retained receipt and quit watch as
single-file exports. One file failing does not prevent later files. Cancellation stops further
media and preserves completed files. A small final manifest is attempted even after an accepted
cancel, so its completed filenames remain attributable; quit waits for this final bookkeeping
and cleanup. The result reports every file plus manifest failure if applicable. Export remains
an explicit host effect outside graph changes, undo, autosave and replay.
