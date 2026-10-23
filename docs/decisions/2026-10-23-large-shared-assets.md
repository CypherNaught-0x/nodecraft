## 2026-10-23 · Assets over 16 MiB travel in parts (#700)

Live sharing ([CRDT sync](2026-10-22-crdt-sync.md)) sent each asset whole in one `AssetPut`
frame, so a card holding a video over 16 MiB never reached the others. Protocol 3 sends such an
asset in parts.

**Two new frames.** `AssetPart { hash, total, offset, bytes }` carries up to 4 MiB of an asset of
`total` bytes; `AssetFrom { hash, offset, len }` asks for `len` bytes from `offset`. An asset may
be up to 8 GiB, the largest one a project imports. An asset of 16 MiB or less still goes whole
as `AssetPut`, so a protocol 2 peer's small assets behave as before; the relay refuses an older
protocol anyway.

**The receiver sets the pace.** Asked for an asset whole (`AssetGet`), a sender with a large
one sends its first 16 MiB in parts. The receiver asks for the next 8 MiB (`AssetFrom`) whenever
less than 8 MiB of what it asked for is still on its way. So at most 16 MiB of one asset is in
flight to one receiver, which keeps under the relay's 32 MiB per-connection queue, and the
sender reads the asset a stretch at a time from its store rather than holding it in memory.

**The relay keeps no parts.** It passes each part on to the askers whose next byte it is.
Each asker takes the parts of one sender, the first whose part arrived; copies from other
senders are dropped. When that sender leaves, the relay asks the remaining editors for the rest
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

Known limits: importing a finished large asset runs on the window's thread and takes a few
seconds per GB; so does a sender's first read of an asset it holds only on disk.
