## 2026-10-27 · A progress window with Cancel for saves and opens, and an advisory disk-space preflight (#367)

Follows [background opens and cancel](2026-10-19-background-open-and-cancel.md), which left these
out.

- **The app's File > Open runs in the background.** Open project…, a recent project, the
  Discard choice and the open after "Save changes before opening?" all dispatch `file.open` with
  `background: true`. The file dialogs hold an `Opening` step until `Session::poll_file` delivers
  the outcome; only then does the app show the new project's canvas and add it to the recent
  list. A failure or cancellation becomes the notice and leaves the current project open.
  An open started by another surface (the control channel, MCP) with `background` is shown the
  same way once it settles.
- **Startup and "Start from the last save" still open inline.** Startup opens before the window
  has a frame to show progress in, and the recovery choice removes its kept copy right after the
  open; making that wait for a background outcome needs its own change to the recovery flow.
- **A modal progress window, shown after 0.3 s.** While a background save or open runs, a modal
  says which step it is on (preparing, writing the file, writing the recovery copy; reading and
  checking the file) with a spinner and Cancel. An ordinary save (~30 ms) finishes before the
  window would appear, so it never flashes. The window owns input, as the file dialogs already
  did during a save: nothing behind it reacts, so no edit queues behind or is refused by the
  running operation. It also appears for an operation another surface started.
- **Cancel only while it changes nothing.** Cancel is offered until a save's file is published
  (then the step reads "Saved. Writing the recovery copy…" and Cancel is disabled with the
  reason) and through an open's checkpoint, which is what makes an open final. Once asked, the
  window says the current step finishes first; the engine's outcome says which side the
  operation landed on, as before. No progress bar: the engine reports phases, not bytes.
- **What a save or open writes, counted before it starts.** `nodecraft_format::space` lists the
  extents each operation creates while the files they replace still exist: a save's sealed
  archive in the private workspace, its staged destination and its new checkpoint (about 3× the
  archive); an open's private frozen copy and its checkpoint (about 2× the file). The workspace
  and journal share the recovery folder's volume; the destination is its folder's volume. When
  both are one volume (same device on Unix, same drive root elsewhere) and both are written to,
  the check uses that volume's peak by phase. Before the destination is published, the prepared
  archive and the staged file coexist with the old file. The atomic rename that publishes the
  new file frees the old one, so the checkpoint written afterwards is charged against that freed
  space: re-saving a project of size A peaks near 2A, not 3A, and a new file (nothing to free)
  near 3A. On separate volumes each is charged everything it holds. An open writes only to the
  recovery volume, so only its free space is checked. Each extent carries 64 KiB of slack for
  blocks and metadata. The save's archive size comes from
  `ArchivePlan::estimate_bytes`: asset descriptors plus the manifest's encoded length, exact to a
  few bytes and never below the prepared archive, without reading any asset.
- **The preflight is advisory and never guesses.** `ProjectStorage::space_probe` is the platform
  service: native storage asks the OS (`fs4`, already pinned for the share spool) for free space
  on each folder; the browser and memory hosts report nothing, and nothing is checked. A save or
  open is refused only when a volume reports less free space than it needs. A failed probe, an
  estimate that can't be made, or a volume reporting zero free (some network filesystems do) is
  treated as unknown and lets the operation go ahead. The check runs on the calling thread before
  any write, history spills included, so a refusal leaves the file, the open project and the
  journal exactly as they were. The error (`busy`) gives the sizes and which drive, never a path,
  and says what to do (free space, or Save As to another drive). Other programs still use space
  after the check; a write that then runs out is handled where it happens, as before.
- **Not counted:** history-only assets spilled before a save, imports already staged, the pack
  that output compaction writes in the recovery workspace when a store has 24 or more small
  backing files (`compact_before_output`), and the extra rewrite when opening an older-format
  file. The session's disk quota bounds those; the inventory doesn't predict them. The old
  recovery checkpoint is never credited: it stays until the new one is published.
- **Quitting during an open cancels it.** Closing the window while a background open runs asks
  it to stop and then waits for the current step, so the window doesn't hang for the whole read
  and the open project is kept. A save still in progress is waited for, as before.
- **Rejected:** a byte-accurate progress bar (needs production stream counters; the phase is
  what can be shown truthfully today), refusing on unknown free space, and a hard reserve
  percentage (a guess that would refuse saves that fit).
