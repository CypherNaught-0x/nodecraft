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
  refuse saves that fit.
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
  as it is decoded (`space::check_open_copies`); an older-format file or a legacy identity is
  rewritten in the current format (`space::check_rewrite`: the archive estimated from
  descriptors, any copies and pack, and its checkpoint). The calibration showed the first one:
  opening a 6 MB classic file wrote about three times its size, not the two the old inventory
  charged. A refusal there cancels the open, so the current project is unchanged. Unknown free
  space or an estimate that can't be made never refuses.
- **Lock order.** The UI thread holds no lock over disk work: a background save or open owns the
  storage outright (`Store::Lent`) and reports through a channel the frame polls with
  `try_recv`; only quitting, and an edit made during a save, wait. Inside format, the order is
  the asset context's operation gate, then an entry's source, then the verified cache, then the
  disk quota's state, which is a leaf and never held across I/O. The cache can be held across a
  pack's write, so it is only ever taken under the operation gate, and every caller that can run
  on the UI thread takes the gate with `try_lock` and treats it as busy: `output_space` reports
  "unknown" rather than wait, imports during an open are refused `Busy` (#475), and the staging
  gate is `try_lock` too. `output_space` reads the cache, releases it, then locks sources, the
  same order verification uses.
- **Calibration** (`persistence_faults367::the_inventory_covers_measured_saves`, a modest real
  project on native storage, measured by sampling the bytes of every file under the test's
  folder while each operation ran, so a peak between samples can be missed): 30 assets of
  200 KiB, a 6,162,420-byte classic-format file. The estimate is the one-volume need the checks
  compare with free space.

  | Operation | Estimate | Measured peak growth | File written |
  |---|---|---|---|
  | Save As from the session that imported the assets | 18,683,948 B | 12.3–16.9 MB over runs | 6,162,420 B (archive estimate exact) |
  | Open of that file in a new session | 18,665,528 B (copy + asset copies + checkpoint) | 18,335,812 B | |
  | Save As from the session that opened it | 18,683,948 B | 18,487,340 B | 6,162,420 B |

  The estimate stays at or above every measurement, by at most the 64 KiB-per-extent slack plus,
  for the first save, the replaced journal (the old checkpoint's space comes back when the new
  one publishes, which the inventory deliberately doesn't credit). The test fails if an estimate
  falls below a measurement or exceeds twice it. Compaction didn't run in these saves (imports
  pack at 30 backing files, so a store rarely reaches a save with 24 or more); a format test
  checks that `output_space` predicts it exactly when an archive does pack. No multi-GiB proof
  was run.
- **Faults tested on the background path:** a save whose file is published and whose checkpoint
  then fails (saved, recovery required, edits refused until a save repairs it); a cancel after
  publication (reports the save); a save cancelled after its archive was prepared (file byte for
  byte, no staged file, the workspace charge back where it was, no cleanup debt); work on a
  background-opened project recovered after its file was deleted (the recovery copy holds its
  media itself); and an agent's MCP commands racing the person's background open (refused as
  busy, naming `file.cancel`, then acting on the opened project) or its own cancelled
  background save (the file unchanged, the work still unsaved).
- **Rejected:** a check against the session's logical disk quota as well (its reservations
  already refuse each extent before writing it, with their own error), and charging cleanup debt
  (it is already on disk).
