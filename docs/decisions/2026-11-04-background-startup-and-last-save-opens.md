## 2026-11-04 · The startup open and "Start from last save" run in the background too (#367)

Follows [the progress window and preflight](2026-10-27-file-progress-and-space-preflight.md),
which left these two opens inline.

- **The startup open waits for the first frame.** `storage_session` no longer opens the project
  named on the command line. It returns that path, and the window's first frame dispatches
  `file.open` with `background: true`, the same path as File > Open. By then every service
  (runs, libraries, templates, skills) is installed against the sample project, so none of
  them is refused `Busy` by a pending open. The progress window shows from that first frame
  rather than after 0.3 s: the open is all the window is doing, and the sample project behind
  it would otherwise flash. Its outcome settles like any background open: on success the file's
  canvas shows and it joins the recent list; a failure or cancellation keeps the sample project
  and says "That project couldn't be opened: …".
- **"Start from last save" discards the copy only after the open has succeeded.** The choice
  starts a background open and moves the offer into the open's pending step. Only a successful
  outcome for this session removes the selected copy and its location record. A failure, a
  cancellation or an open that ends without an outcome leaves the copy byte for byte, keeps the
  current project, says the work was kept, and asks the question again so Recover is still one
  click away. When the host replaced the session while the open ran, the copy is kept and the
  question is dropped with the session it was asked over. A cancel that arrives after the open
  published its checkpoint gets the open's real outcome (success), so the copy is then resolved.
- **The startup question waits for a running open.** While any background save or open runs, the
  recovery question isn't drawn; the progress window owns input. It appears once the open has
  settled, so a crash followed by a launch naming a file shows one modal at a time.
- **Still inline, and why.**
  - *The first session checkpoint.* The desktop session always starts from the sample project,
    a few kilobytes, before the file opens; nothing large is written there.
  - *Reading a kept copy to ask the question.* `RecoveryOffer::read` replays the kept journal
    before the window exists, because the question shows what it holds (title, change count,
    damage). Moving it needs an asynchronous offer state; the replay streams and is bounded by
    the journal's own limits.
  - *Recover.* `Session::recover` checkpoints the recovered snapshot on the calling thread (about
    one archive's bytes, against an open's two). Running it on the runner needs a third
    background operation in `Persistence` with its own phase, `Busy` and cancel semantics
    (cancel before the checkpoint publishes keeps the copy and the current project), plus the
    identity migration and asset-context rebinding that `replace_valid` does on the UI thread
    today. That is engine work of its own, not a reuse of the open path, and is left for a
    follow-up rather than approximated here.
- **Rejected:** opening at startup inside the constructor with the runner (services installed
  afterwards would be refused while the open is pending), and removing the copy when the open
  *starts* (a failed or cancelled open would lose the work the person was offered).
