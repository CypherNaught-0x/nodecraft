## 2026-10-22 · Share links, roles and signed discussions (#121)

A project shared live ([CRDT sync](2026-10-22-crdt-sync.md)) can now be opened through a **share
link** that lets its holder view it, comment on it or edit it, without giving away the room id.

**Whoever has the room id is the owner.** They edit, and they alone make, change and take back
links. A link is a `LinkKey`: 32 random bytes in hex, drawn by the owner's session. The relay
never sees a key. It knows each link by the key's blake3 hash, so a relay's memory (or a leaked
`share.links` list) holds no working link. `LinkKey`'s `Debug` output is redacted, as
`RoomId`'s is, and neither appears in a log or an error.

**A link carries one `Grant`**: a role (`view`, `comment` or `edit`; never `owner`), the name of
the person it is for, and an optional expiry in seconds since 1970 (UTC).

- **No passphrase.** A 256-bit key already can't be guessed, and a passphrase sent beside it
  would travel the same way. Expiry and revocation are the controls instead.
- The relay checks expiry with its own clock, at least once a second. An expired link is
  dropped, and whoever is in through it is put out.
- Changing a link's terms puts out whoever is in through it; they come back on the new ones.
  Revoking a link puts them out at once. After that, nothing reaches them: no updates and no
  assets.

**The relay keeps links only while the room is open.** The relay keeps nothing on disk. The
owner's `Collab` holds the links and sends them again on every connection, with its 64 latest
revokes (another device of the owner's may still hold a revoked link). An expired link is
dropped from the owner's list too. A link therefore
works only while its owner, or someone else in the room, is connected. Making links last is
left to whatever later keeps rooms open with nobody in them.

**The relay enforces roles; the session mirrors them.** Each peer in a room is a `Member` with a
role.

- An **editor** joins as the owner does: the relay sends it a `Step1`, and it writes the
  shared document directly.
- A **viewer** or **commenter** is never asked for its document. After the `Welcome`, it
  sends its own `Step1` and takes the project in.
- If a viewer or commenter sends a `Step2` or an `Update`, the relay hangs up on it.
- Media from a viewer or commenter is dropped, since it can't have added any. Their asks for
  media go only to editors.

`Collab::settle` gives the session its role as an `engine::access::Access`. Every local
changeset is checked against it before it is staged:

- a **viewer** may make no change at all;
- a **commenter** may make only the eight discussion changes;
- runs are refused unless the role edits;
- changes coming in from the shared document are not checked, because the relay has already
  checked them.

**Commenters don't write the shared document.** For each discussion it changed, a commenter
sends a `CommentPut` holding the thread as it was (`base`) and as the commenter made it. The
relay then:

1. merges the change three ways (`nodecraft_sync::discussion::merge`) into the thread as it is
   now, refusing any part that isn't the commenter's to make;
2. writes the result as its own yrs client;
3. sends the resulting update to everyone, the commenter included;
4. sends the commenter `CommentDone { seq, refused: None }`.

A put that isn't the commenter's to make is dropped, and the relay answers
`CommentDone { seq, refused: Some(why) }` without putting them out: an honest client can send
one after an undo or a reconnect. A new thread sent again after a reconnect, which the relay
already wrote, changes nothing. A commenter's session refuses to put a whole thread back (by
undoing its delete) unless every message in it is theirs.

Until the done arrives, the commenter's session shows its own changes laid over what the
shared document holds, so nothing flickers. A reply someone else sent meanwhile is kept. If the
canvas has gone, the change is dropped and the commenter is not put out.

**What a commenter may do** is decided by **authors**:

- **New messages are signed.** `Comment` and `CommentReply` gain an optional `author`: at most
  128 bytes, not blank, with no control characters. A session's new messages carry its share
  name, so a commenter's always do.
- **Only a message's author may reword it.**
- **Moving or deleting a message** is allowed to its author, or to a role that edits.
- **Anyone who may comment** may resolve or reopen a thread, and reply to it.

An owner's own messages carry no name. Messages that don't name an author belong to whoever has
no name, which is the owner.

**Authors bind commenters, not editors.** An editor writes the shared document directly, so
the relay doesn't check what it signs: an `edit` link is trusted as the owner is, and its
session's checks are a courtesy, not a guard. Names aren't unique within a room either; two
links for "Ana" write as the same person. Give `edit` only to people you would hand the room
id.

**`PROTOCOL` is 2.** The new frames are:

- `HelloLink` and `Welcome`;
- `LinkPut` and `LinkRevoke`, which only the owner may send;
- `CommentPut` and `CommentDone`.

They have JSON bodies (`CommentDone` is its sequence number and a reason), and all but
`CommentPut` are capped at 4 KiB. A room holds at most 256
links. A relay refuses a `LinkPut` whose hash another room already holds.

**Commands.** These go through the host service `share.links`, which a host registers while
it shares a session and passes to `Collab::links_request`:

- `share.links` lists the links (agents may use it);
- `share.link_create` makes a link, and shows its key only then (human only);
- `share.link_update` changes a link's terms (human only);
- `share.link_revoke` takes a link back (agents may use it).

A session that isn't shared returns *unsupported*. A refused role check has its own error kind,
`forbidden`.

**Not done here:** private canvases ([their own record](2026-10-22-private-canvases.md)), the desktop Share and Join UI, and
presence.
