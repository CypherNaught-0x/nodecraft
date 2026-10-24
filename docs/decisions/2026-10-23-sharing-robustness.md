## 2026-10-23 · Live sharing: four robustness gaps closed (#701)

The review of #119 left four small gaps. Each is closed in the place it arose.

**An undo that a peer took away is dropped.** A peer's edit can remove what one of this peer's
steps needs, such as the card a step would put back. The step then can't apply. Before, it
stayed at the top of the stack and blocked every undo below it. Now each step records whether a
peer's edit came after it. A step that fails to apply, and was overtaken by a peer, is taken off
its history, and the command says so ("…it was taken off the undo history"). The next undo
reaches the step below it. A step that fails with no peer's edit since stays where it is: that
is a defect, not someone else's change, and dropping it would hide it.

**A room measures what its document holds.** The relay counted every merged update toward a
room's 512 MiB, forever, so a long session of edits filled the room even though yrs collects
replaced and removed content as it goes. Now, once the count reaches the limit, the room encodes
its document to learn its real size, and counts from there. Measuring is a pass over the whole
document, so it happens at most once per eighth of the limit taken in. A room whose document
really is that large still refuses what would add to it, as before. Restarting the room's
document from a fresh snapshot was not chosen: every peer would have to re-join it.

**Silent connections are limited per address.** One address may hold at most 16 connections
that haven't said which room they want. Past that, more from it are closed at once, until some
do or their 10 s run out. Behind a TLS proxy every peer has the proxy's address, so 16 people
can be joining through it at the same moment; once in, they don't count.

**A panic in the browser stops the app and says so.** The command guard catches a panic on the
desktop and keeps the document. In the browser a panic aborts (wasm has no unwinding), so there
is no guard to keep anything. The page now shows that NodeCraft stopped and to reload it, instead of a
canvas that no longer answers. The no-panic rules remain what keeps a browser peer safe from a
remote changeset: every remote edit is staged, validated and admitted the same way as a local
one.
