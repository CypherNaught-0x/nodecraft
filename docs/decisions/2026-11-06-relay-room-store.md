## 2026-11-06 · The relay can keep its rooms on disk (#1068)

Until now the relay kept nothing on disk ([`2026-10-22-crdt-sync.md`](2026-10-22-crdt-sync.md)) and
a share link worked only while someone was in its room
([`2026-10-22-share-links.md`](2026-10-22-share-links.md)). That record left "making links last"
to whatever later keeps rooms open with nobody in them; this is it.

- **Opt in, with a folder.** `nodecraft-cli relay --store <folder>` keeps rooms there. Without it
  the relay behaves as before: nothing on disk, and a room goes when its last peer leaves.
- **What is kept is what the room holds.** For each room: its document as one yrs update, its
  share links with their terms (role, name, expiry), and the media it keeps for late joiners.
  Links live in the same per-room state as the document, so a revoke or an expiry removes the
  link from what is kept, exactly as it removes it from the room.
- **No key on disk.** A room's folder is named by a keyed hash of its id
  (`blake3::derive_key("nodecraft relay room key 2026-11-06")`), and the relay's memory uses the
  same hash, so the id it receives in a hello is never written. Links are kept by their hash,
  as the room already held them. Media files are named by their content hash.
- **Atomic, single writer.** One saver thread writes the store: a room that changed is written
  every 5 s, and a change to its links wakes the saver at once, so a revoke reaches the disk
  within moments. Every file is written to a temporary file, synced and renamed over the old one;
  the links file is written last. A relay killed mid-write keeps the last complete copy. A
  revoke the relay accepted less than a moment before it was killed can come back; the owner's
  session sends its latest revokes again on its next connection, as it already did.
- **Bounded, and forgotten when unused.** At most 256 rooms and `--store-max-gib` (default 8)
  bytes; a room nobody joined or changed for `--keep-days` (default 30) is forgotten, checked at
  start and hourly. A room is kept only once its owner has sent something, so a hello with a
  made-up id writes nothing. A new room takes only free space: anyone can open one, so new rooms
  never push kept rooms out. A kept room that grows past the bounds lets the rooms used longest
  ago go first, never a room someone is in. A room that can't be kept isn't, and the relay says
  so once on stderr without naming it.
- **Failing closed.** When saving a kept room fails (the store is full, the disk is), its older
  copy is deleted rather than left: it could hold a link revoked since. The room stays in
  memory while someone is in it, and is tried again at its next change.
- **Never an empty room over a kept one.** A join reads the room back with the rooms held if it
  left memory between the relay's two looks; if the relay is too full to hold it, the joiner is
  refused for the moment instead of being given an empty room. Each room is still capped as in memory (512 MiB document, 512 MiB
  media).
- **Read back as hostile.** Sizes are checked before reading; a document that doesn't merge, or a
  links file that doesn't parse, forgets that room; links that aren't valid grants, media whose
  bytes don't match their hash and anything past a room's limits are left out. Folders and files
  that aren't rooms are left alone.
- **Cost.** A changed room is encoded whole under the relay's lock and rewritten whole, at most
  every 5 s; a very large, busy room costs every other room that pause. Incremental writes are
  left for when that shows up.
- **Rooms come back on demand.** The relay reads only each room's links at start. A room's
  document and media are read when someone joins it, by its id or through one of its links,
  without holding the other rooms. An empty room leaves memory once the store has all of it.
