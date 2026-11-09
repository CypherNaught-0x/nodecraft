## 2026-11-06 · Large project persistence is off the UI thread: #367's acceptance, met (#367)

[Recover in the background and what is left of #367](2026-11-04-background-recover-and-close-out.md)
mapped #367's acceptance to its evidence and left two rows "not yet", both for the startup
keep-aside. [The startup keep-aside runs off the UI thread](2026-11-05-startup-keep-aside-off-the-ui-thread.md)
(#1053, PR #1057) closed most of them; this change closes the two gaps an independent review
found left (below). The earlier record carries only a one-line update pointing to #1053; this one
says where each row now stands, so the issue can close on evidence rather than on a summary.

**What this change adds.**

- **The first checkpoint is written off the UI thread too.** Taking the journal over used to run
  `Session::attach_storage` on the UI thread once the keep-aside settled, and that writes the
  session's first checkpoint (replacing the journal) with a cancel nothing could reach. Now the
  keep-aside's own thread does that disk work, `Session::prepare_storage` over
  `Persistence::open_in`, from the document the held session shows (it can't change while held),
  and the UI thread only installs it: `Session::attach_prepared` calls `Persistence::assemble`,
  which writes nothing, and refuses (`Conflict`) a session that changed anyway, rather than give
  it a checkpoint without its change. `attach_storage` stays, built on the same two steps.
- **Each earlier copy's replay is charged.** When the live journal holds no work, the scan of
  earlier kept copies (`RecoveryOffer::earlier`) replays up to `MAX_KEPT` (99) of them, and each
  replay copies its checkpoint into the private workspace. Each is now checked against
  `SpaceInventory::replay_journal` first, as the live journal's replay is. A copy whose replay
  wouldn't fit stops the scan: the window still takes the journal over, says the earlier work
  wasn't checked and why (the sizes, never paths), and the copies stay to be offered another
  time.
- **Still not cancellable: the first checkpoint itself.** It is the starter document, written once
  the journal is already kept, so a "Decide later" or quit that lands during it loses nothing;
  quitting waits for it at most `QUIT_WAIT`, as for the rest of the keep-aside. The progress
  window shows the last copy step meanwhile.
- Tests: `attach_storage1053::storage_prepared_on_another_thread_is_attached_without_writing_and_only_over_the_same_document`,
  `keep_aside::tests::an_earlier_copy_whose_replay_would_not_fit_is_left_for_next_time`.

| Acceptance | Where it stands |
|---|---|
| Native off-UI worker, bounded queued operations, exact identity, one owner, truthful progress and cancel; AssetContext `Busy` and lock order kept; no UI or runner lock held over disk work or callbacks | **Met.** Saves (#461), opens (#475), startup and last-save opens (#1045), recover (#1054) and now the startup keep-aside (#1057): a thread owns the storage and its lock and writes the first checkpoint (`Session::prepare_storage`, this change), the window starts without it and `Session::attach_prepared` installs it once the keep-aside settles, writing nothing on the UI thread, with `Session::hold_for_storage` refusing changes as `Busy` meanwhile. Tests: `keep_aside::tests::the_first_frame_shows_the_keep_aside_and_frames_go_on_while_it_is_held`, `…::quitting_while_it_is_held_or_dropped_keeps_every_byte`, `…::quitting_after_it_finished_but_before_a_frame_took_it_in_decides_the_journal_as_usual`, with the evidence listed in the earlier record for the other operations. |
| Inventory every simultaneous extent and body pass; phase bounds; actionable errors without paths; preflight advisory; real write errors handled | **Met.** The keep-aside's copies are charged: `SpaceInventory::replay_journal` (each replay's private copy, `Extent::ReplayCopy`: the live journal's and, with this change, each earlier copy's) and `SpaceInventory::keep_journal` (the kept copy, `Extent::KeptJournal`) against `ProjectStorage::recovery_free`, refused as `Busy` with sizes and no paths (`crates/format/src/space.rs`; `keep_aside::tests::a_journal_the_recovery_drive_cannot_copy_is_refused_and_kept_in_place`). The rest as in the earlier record. |
| Cancel before publication leaves old authority unchanged; after publication can't report rollback; checkpoint failure truthful; latch and debt owned; partial append requires a checkpoint; bounded chunks, no promise for a blocking read | **Met.** The keep-aside adds: a cancel checked before each 1 MiB chunk, after the sync and throughout the replay; the copy written to `<slot>.partial` and renamed only once synced; the live journal only ever read (`…::a_cancel_lands_at_the_next_chunk_and_removes_the_partial_copy`, `…::decide_later_keeps_every_byte_and_leaves_the_journal_for_the_next_launch`). |
| Self-contained recovery, sealed-source identity, source deletion safety, history roots and pins, quota and debt accounting; no pathname checkpoint or reflinks | **Met**, as in the earlier record; the kept copy is a real copy (`…::recover_after_a_background_keep_aside_brings_the_work_back`). |
| Real native Session and storage paths with slow, error and cancel boundaries, concurrent and mismatched operations, publish then recovery failure, cleanup debt, recovery after source removal; UI and event pump progress independent of provider traffic; memory streaming and wasm unsupported behaviour kept | **Met**, as in the earlier record, plus the keep-aside's own: a damaged earlier copy skipped off the UI thread (`…::a_damaged_earlier_copy_is_skipped_off_the_ui_thread`), the question built from the one replay (`…::the_question_is_built_from_the_replay_that_decided_to_keep_the_journal`). Web recovery still runs inline (`web_journal`), unchanged. |
| Old compatibility and version semantics, physical evidence; modest real calibration plus synthetic controls; heavy multi-GiB proof only with explicit resource authorization | **Met within what was authorized.** Formats unchanged; calibration in [the inventory record](2026-11-04-persistence-inventory-and-lock-order.md). No multi-GiB run was authorized, so none was made: that is the one proof this acceptance makes conditional, and it stays a choice for the owner. |

The earlier record's "repeated replays of kept copies bounded" is met by #1053's scan: at most
`MAX_KEPT` (99) copies, newest first, off the UI thread, cancellable before and inside each
replay, and now each charged. Quitting during a save still waits for that save on the closing
thread, as #475 and the recover record decided.

Not part of #367, and not done by closing it: a byte-accurate progress bar (phases only, as #475
decided), crash-orphan workspace cleanup (#363) and format-family coordination (#359), which #367
reuses and does not replace.
