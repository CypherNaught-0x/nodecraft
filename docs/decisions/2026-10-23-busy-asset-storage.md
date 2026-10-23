## 2026-10-23 · Work on a project's storage while a shared asset is sealed (#855)

The sharing worker seals a large arrival on a clone of the session's store
([large shared assets](2026-10-23-large-shared-assets.md)). The seal holds the project's storage
for as long as the copy and hash take, seconds for a large video, and other storage work is
refused as Busy meanwhile. This is kept, rather than narrowing the seal's lock to its accounting
steps: the lock is what keeps packing and verification from moving the bytes a seal or a read
is using, and narrowing it would mean re-proving those paths. Instead, what the window and the
worker do while storage is busy changed.

**Small writes wait in memory.** On a host with private storage, an asset of 16 MiB or less
(`AssetStore::insert`: a model's result, a paste, a drop, a font) that finds the storage Busy is
kept in memory instead, as a browser session keeps every asset. At most 48 MiB is kept like this
in all; past that the insert is Busy as before. The staging pool reports Busy too, not Conflict,
since it is the same condition. Packing the store after an import is skipped while busy and done
at a later import. The next insert that finds the storage free moves what waited in memory to
disk, so the 48 MiB is room for one busy spell, not for the session. A store with private storage
reads what waits in memory from memory each time rather than caching it, since packing counts
only the cache's disk backings. Still needing the storage: an asset over 16 MiB, and one whose
bytes the project already holds, which is checked against the stored copy before it is reused.

**A busy ask waits.** An ask for an asset this session can't read because the storage is busy
is kept (bounded like other asks) and asked again on the worker's two-second retry, rather than
dropped. Other read errors still drop it, as before.

**One seal a turn, and Stop cancels it.** The worker seals one finished arrival each turn,
starting after the last one that had to wait so one can't hold up the rest, and asks for another
turn while more remain, so edits are published and taken in between. Its
seals carry a `TransferCancel` that dropping the worker trips, so a Stop ends a seal under way
within a read, and the arrival is left as it was.

Cancelling the seal of an asset no card uses any more is not done here.
