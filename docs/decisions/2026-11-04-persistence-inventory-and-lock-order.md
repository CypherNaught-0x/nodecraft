## 2026-11-04 · The space inventory counts housekeeping and rewrites; lock order and calibration (#367)

Follows [the progress window and preflight](2026-10-27-file-progress-and-space-preflight.md),
whose inventory left out history spills, staged imports, compaction packs, older-format rewrites
and cleanup debt.

- **Phases, not a sum.** `space::SpaceInventory` now gives each extent the phase it is written
  in (housekeeping, prepare, stage, publish) and takes the peak over phases, per volume and, when
  the destination and recovery folders share one, for the volume as a whole. The file a save
  replaces is credited from publication on, as before. A transient extent exists only during
  its phase.
- **Housekeeping a save does first is counted.** Before an archive is written, every asset it
  reads must be verified in the private workspace. An asset decoded from a project file whose
  media haven't been verified there yet is copied first, and history-only roots are spilled the
  same way. `AssetStore::output_space` adds those bytes up from descriptors, without reading any
  asset, as `Extent::VerifiedCopies`: they stay, since the archive is written from them. In the
  calibration below the term was 0 (imports and opens verify as they go); it guards the cases
  where they haven't.
- **The compaction pack is transient.** When the workspace holds 24 or more backing files (each
  copy is one), output packs every verified backing into one file, then frees the originals. The
  pack (cached bytes plus the new copies) is charged during housekeeping only: once written it
  replaces what it packed. Charging it for the whole save would double-count the copies and
  refuse saves that fit. Two approximations remain, both inside housekeeping: when the history
  roots' verification packs first, the later pack may not run, so the charge can be one pack too
  many (an over-count, only when housekeeping is the peak); and an original a reader still leases
  outlives the pack, so its bytes coexist with the pack longer than charged (an under-count).
- **Extents that already exist aren't charged.** The open project's private copy, imports
  already staged, history pins and files whose cleanup failed are on disk before the save
  starts, so they are already missing from the free space the platform reports; charging them
  again was the over-count the #1004 review corrected. Cleanup debt is named in a refusal on the
  recovery drive ("closing and reopening NodeCraft frees them"), since that is what the person
  can do about it. The previous checkpoint is still never credited.
- **What an open writes after reading the file is checked when it is known.** An open's first
  check (before anything is written) charges the private copy and the checkpoint. Two more
  writes are known only once the copy is read, so they are checked then, inside the open,
  against the free space left after the copy was made: a classic-format file (any project small
  enough for the classic wire, up to 64 MiB) has each asset copied and verified in the workspace
  as it is decoded (`space::check_open_copies`, which skips media the workspace already holds
  verified and names cleanup debt, as `check_rewrite` does); an older-format file or a legacy
  identity is rewritten in the current format (`space::check_rewrite`: the archive estimated
  from descriptors, any copies and pack, and its checkpoint). The calibration showed the first
  one: opening a 6 MB classic file wrote about three times its size, not the two the old
  inventory charged. A refusal there cancels the open, so the current project is unchanged and no
  checkpoint is published. Unknown free space or an estimate that can't be made never refuses.
- **The checks are tested where they are wired.** `NativeStorage::set_space_probe` lets a host
  report free space itself; `space_wiring367` uses it to make the recovery drive short at exactly
  one check, on real Session paths: a classic open refused once its file is read, a
  legacy-identity open and a v1 open refused at their rewrite, and a save refused because its
  history roots alone don't fit. Removing any one of those calls fails its test (checked by
  hand).
- **History roots are verified on the save's own thread.** A background save used to copy its
  history-only roots (and possibly compact) on the calling thread before starting, holding the
  asset gate and not cancellable. That now runs first inside the job, under the job's
  `TransferCancel`, before anything else is written, so `file.cancel` stops it and the frame
  never waits on it. An inline save (no runner) still does it on the calling thread, as all of
  its work.
- **Lock order.** For background saves and opens the UI thread holds no lock over disk work: the
  job owns the storage outright (`Store::Lent`), does all of its copying (history roots
  included) itself, and reports through a channel the frame polls with `try_recv`; only quitting,
  and an edit made during a save, wait. The preflight on the calling thread reads descriptors and
  free space only. Inside format, the order is
  the asset context's operation gate, then an entry's source, then the verified cache, then the
  disk quota's state, which is a leaf and never held across I/O. The cache can be held across a
  pack's write, so it is only ever taken under the operation gate, and every caller that can run
  on the UI thread takes the gate with `try_lock` and treats it as busy: `output_space` reports
  "unknown" rather than wait, imports during an open are refused `Busy` (#475), and the staging
  gate is `try_lock` too. `output_space` reads the cache, releases it, then locks sources, the
  same order verification uses.
- **Calibration** (a modest real project on native storage, measured by sampling the bytes of
  every file under the test's folder while each operation ran, so a peak between samples can be
  missed; the estimate is the one-volume need production compares with free space, computed the
  way production computes it).
  - Classic (`persistence_faults367::the_inventory_covers_measured_saves`): 30 assets of
    200 KiB, a 6,162,420-byte file.
  - Streaming (`space_wiring367::a_streaming_project_calibrates_the_copies_term`): 4 assets of
    13 MiB, a 54,528,746-byte v4 file, opened in a new session (its media stay indexed) and
    saved again, which copies all of them first.

  | Operation | Estimate | Measured peak growth | File written |
  |---|---|---|---|
  | Classic: Save As from the importing session | 18,683,948 B | 12.3–18.5 MB over runs (a reviewer measured 18,487,340 B) | 6,162,420 B (archive estimate exact) |
  | Classic: open in a new session | 18,665,528 B (first check, then copies + checkpoint) | 18,335,812–18,468,920 B | |
  | Classic: Save As from the opened session | 18,683,948 B | 18,487,340 B | 6,162,420 B |
  | Streaming: open in a new session | 109,188,644 B | 109,057,572 B | |
  | Streaming: Save As from the opened session (copies 54,525,952 B) | 218,374,414 B | 163,583,524 B | 54,528,746 B (exact) |

  The estimate stays at or above every measurement. The excess is the 64 KiB-per-extent slack
  plus, for saves, the journal the new checkpoint replaces: its space comes back when the new
  checkpoint publishes, which the inventory deliberately doesn't credit (about one archive for
  the streaming save). The tests fail if an estimate falls below a measurement or exceeds twice
  it. Compaction didn't run in these saves (imports pack at 30 backing files, so a store rarely
  reaches a save with 24 or more); a format test checks that `output_space` predicts it exactly
  when an archive does pack. No multi-GiB proof was run.
- **Faults tested on the background path:** a save whose file is published and whose checkpoint
  then fails (saved, recovery required, edits refused until a save repairs it); a cancel after
  publication (reports the save); a save cancelled after its archive was prepared (file byte for
  byte, no staged file, the workspace charge back where it was, no cleanup debt); work on a
  background-opened project recovered after its file was deleted (the recovery copy holds its
  media itself); and an agent's MCP commands racing the person's background open (refused as
  busy, naming `file.cancel`, then acting on the opened project), its own background save
  cancelled before its job ran (the file unchanged, the work still unsaved) and cancelled while
  its job was already writing (reported as saved); and a control-channel client racing the
  person's background open (busy, reads still answer, then the opened project).
- **Rejected:** a check against the session's logical disk quota as well (its reservations
  already refuse each extent before writing it, with their own error), and charging cleanup debt
  (it is already on disk).
