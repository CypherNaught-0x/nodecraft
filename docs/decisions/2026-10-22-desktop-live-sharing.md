## 2026-10-22 · Live sharing from the desktop app (#698)

The sync engine ([CRDT sync](2026-10-22-crdt-sync.md)) and its share links
([share links](2026-10-22-share-links.md)) can now be used from the desktop app, not only from code.

**Commands ask a host service.** `share.start {relay?}`, `share.join {link}`, `share.stop` and
`share.status` check their parameters, then ask the `share.session` service. The service only
queues the request; the app takes it up on its next frame. A host without the service (the CLI,
the browser for now) answers *unsupported*. `share.start` and `share.join` decide who sees the
project and whose project this window opens, so they are human only, like `share.link_create`;
agents may read `share.status` and run `share.stop`.

**A join link is one line of text:** the relay's `ws://` address, then `#room=` or `#link=` and
64 hex digits. What follows the `#` is the capability. A room id joins as the owner; a share
link's key joins in that link's role.

- `JoinLink`'s `Debug` output leaves the key out, no error repeats a link, and `share.status`
  never returns one.
- The panel hides the link until **Show** is pressed; **Copy join link** copies it without showing
  it. Only links made by sharing (room ids) are shown; a joiner's own key is not.
- The Join field is masked.

**Only `ws://` for now.** `wss://` is refused as *unsupported*: TLS would add a TLS stack to the
desktop build, and the relay doesn't serve it yet either. A relay address may not hold `@`
(no credentials in the address), `#`, whitespace or more than 2 KiB. The relay last shared
through is kept in settings (`[share] relay`) and fills the field next time.

**The connection has its own thread.** It speaks tungstenite over a plain TCP stream, with frames
and messages capped at the protocol's `MAX_FRAME_BYTES`. The UI thread never waits on the
network: each frame it takes what came, settles the session and hands over what to send.

- When the connection drops, the project stays open and editable. The thread tries again after
  1 s, doubling to at most 30 s, and the chip says *Reconnecting…*.
- Each connection has a number. Frames the app made for an earlier connection are dropped, so
  the next connection always starts with its hello, which catches both sides up.
- A quiet relay is pinged every 10 s. If nothing at all comes back for 30 s, the connection is
  taken for lost, so a half-open socket (a laptop that slept, a NAT that forgot the mapping)
  reconnects instead of showing *Live* while nothing moves.
- Frames waiting for the app are capped at two frames' worth of bytes. Past that the thread
  stops reading until the app catches up, and the app takes at most one frame's worth per
  frame. Reading stops after 16 messages to send what is waiting, so a busy room never holds
  this side's changes back.
- A joining window is view-only until the shared project arrives, since anything made there
  would be replaced. Sharing ends if another document replaces the shared one in its window,
  even another copy of the same project.
- Connection errors are described in words, never with the relay's address.

**Joining opens a new window.** The app starts itself again with `--join` and writes the link to
the new process's standard input, never its command line, where other people on the machine
could read it. The joined window opens without the person's project storage, so the shared
project doesn't take over their recovery journal or last-opened project; it lives in memory.
That window starts with the starter project and takes up the shared one when its first update
says which project it is (`Session::adopt_joined`, refused once the window has its own edits).

**Stopping keeps the project.** `share.stop` ends the connection and gives the session the
owner's access again; the project stays as it was. Opening another project in a window that
shares ends the sharing, so that project never reaches the room.

**Not done here:** `wss://`, the browser's transport, saving a joined project to a file, and
presence.
