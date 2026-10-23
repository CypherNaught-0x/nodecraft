## 2026-10-23 · Assets over 16 MiB travel in parts (#700)

Live sharing ([CRDT sync](2026-10-22-crdt-sync.md)) sent each asset whole in one `AssetPut`
frame, so a card holding a video over 16 MiB never reached the others. Protocol 3 sends such an
asset in parts.

**Two new frames.** `AssetPart { hash, total, offset, bytes }` carries up to 4 MiB of an asset of
`total` bytes; `AssetFrom { hash, offset, len }` asks for `len` bytes from `offset`. An asset may
be up to 8 GiB, the largest one a project imports. An asset of 16 MiB or less still goes whole
as `AssetPut`, so a protocol 2 peer's small assets behave as before; the relay refuses an older
protocol anyway.

**The receiver sets the pace.** Asked for an asset whole (`AssetGet`), every editor with a
large one sends only its first part, since only the first to arrive is taken. The receiver asks
that sender for the next 8 MiB (`AssetFrom`) whenever no more than 8 MiB of what it asked for is
still on its way. So at most 16 MiB of one asset is in flight to one receiver, which keeps under
the relay's 32 MiB per-connection queue, and the sender reads the asset a stretch at a time from
its store rather than holding it in memory. A sender answers at most two asks per publish and
the rest at the next, so a flood of asks can't make it read gigabytes at once.

**The relay keeps no parts.** It passes each part on to the askers whose next byte it is.
Each asker takes the parts of one sender, the first whose part arrived; copies from other
senders are dropped. An asker's `AssetFrom` is passed on only if it follows on from what it
asked for before and stays within two windows of what it has, so a viewer can't ask an editor
for an asset many times over; an `AssetGet` starts that asker's transfer over. When that sender leaves, the relay asks the remaining editors for the rest
(`AssetFrom` from the asker's next byte), and a newcomer is asked for what nobody is sending.
Small assets are kept by the room as before; parts are not, since a room's 512 MiB couldn't hold
many large ones.

**The receiver checks the whole asset.** Parts are taken only in order, hashed as they come,
and kept in a `Spool` until the last arrives. If the hash doesn't match, the asset is dropped
and isn't asked for again; the cards that use it wait, and the status counts it as a file that
can't come. A receiver that reconnects keeps what it has and asks only for the rest. The default
spool keeps parts in memory and takes assets up to 16 MiB, which is all a browser session can
import; the desktop keeps them in temporary files, up to 16 GiB at once, deleted once imported
or given up. A finished asset is imported from its file (`Session::import_asset_from`).

**Progress is shown.** `share.session` `status` reports `asset_bytes_received`,
`asset_bytes_total` and `unusable_assets`, and the chip shows the percentage while files are on
their way.

Off the window (#842): the desktop's sharing thread seals a finished large asset into the
project's private storage on a clone of the session's store (`AssetStore::seal`, under the
storage's operation lock, so a save or another import meanwhile is told it is busy), and the
plan only adopts the sealed result (`Session::adopt_sealed_asset`), a map insert. A sender
reads its parts on that thread too. Each spool file is locked while its spool holds it, and a
new spool deletes unlocked ones older than a minute, which a crash left behind. A spool that
can't write (a full disk) puts the asset off and asks again two seconds later, up to eight
times before giving it up; the spool's budget is also bounded by half the disk's free room
beyond 1 GiB, since sealing copies the asset again, so an asset over it waits for room instead
of filling the disk. Free room comes from the `fs4` crate (pinned), as std has no safe call
for it.

Known limit: an editor who sends wrong bytes first makes the asset unusable for that receiver
until it reconnects; asking another sender needs a relay frame of its own (a follow-up to
#842).
