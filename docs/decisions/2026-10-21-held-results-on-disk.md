## 2026-10-21 · Held results are saved one record each, beside the run journal (#614)

- **One record per held result.** A held result (#369) is saved when it is held and removed
  when it is recorded, placed or discarded. It is never rewritten. The run journal is rewritten
  on every usage change, so held media don't go in it.
- **Record format.** A magic line `NCHELD1`, a little-endian `u32` header length, a JSON header,
  then the raw bytes of each inline output back to back. The header holds the project, canvas,
  node, provider, model, reason, routes, recipe and usage. It lists the outputs, with the length
  of each one's bytes. The desktop keeps records as `held/<n>.held` beside `runs.json`, under the
  same lock. `Output::Url` outputs keep only their URL, which may expire. A signed URL is a
  short-lived credential for that one file. It is saved as it came, in the user's own data
  folder, like the journal's provider job ids.
- **Written outside the held lock.** The record is encoded first. Its number and size are
  reserved under the lock, and then it is written without the lock held, so a large record never
  stalls the runner checking whether the held list is full.
- **Bounded.** All records together take at most `MAX_HELD_STORE_BYTES` (1 GiB). A result that
  would go over, or that can't be written, is still held, in memory only. The queue panel marks
  it "until quit", and `run.held` reports it with `saved: false`. At most `MAX_HELD` records are
  restored per launch, and at most the byte cap's worth. Any others wait on disk for a later
  launch, and they still count against the cap. Records are restored from every project, so a
  launch can open with the held list full and new runs waiting. The queue panel lists each held
  result's card, so the user can place or discard it.
- **Record numbers are never reused.** A new record gets a number above every record in the
  folder, set-aside ones included. Only canonical names (`7.held`, not `007.held`) are records.
- **Damaged records are set aside.** A record that can't be read is renamed `*.held.damaged`
  (`*.held.damaged-<k>` if that name is taken). It is never overwritten. Opening the store
  reports it, and every other record is restored. A record from a newer NodeCraft is left in
  place, unread, for that version to restore.
- **A record that can't be removed comes back** after a restart, still held. Placing it twice
  makes two copies on the canvas. Nothing is sent or charged again.
- **wasm and the CLI** have no store, so held results stay in memory there.
