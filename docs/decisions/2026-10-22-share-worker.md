## 2026-10-22 · Live sharing merges off the window's thread (#714)

[Desktop live sharing](2026-10-22-desktop-live-sharing.md) read frames on its own thread, but
decoding them, merging them into the shared document and working out what the session must take
(materialise and diff) still ran on the window's thread, every frame. A joiner catching up on a
large project froze the window for as long as that took. The wire protocol is unchanged.

**The merge has its own thread.** It owns the connection and the `Collab`: it decodes and
receives frames, answers them, publishes local edits, materialises the shared document and
diffs it. The window keeps only the session.

- Each frame the window may hand it a *turn*: the session's revision, a copy of its document
  (only when the revision changed since the last turn), the hashes of its assets, and what came
  of the last plan. One turn is in flight at a time.
- The thread answers with a `Plan` (`Collab::plan`): the access the room gives this session,
  the assets that arrived, and what brings the document, as it was at the turn's revision, up to
  the shared one. The window takes it with `Plan::take` and tells the thread what came of it at
  the next turn (`Collab::taken`). `Collab::settle` is the same three steps on one thread, for
  hosts that have no other.
- Asset bytes the others ask for are in the session, so the thread names them and the window
  sends them; the thread never holds the session.
- The owner's share links stay on the window (`collab::Links`), so `share.links` answers at once;
  what they send goes to the thread to go out.
- Making the `Collab` for a large shared project takes a while too, so the thread does that as
  well, from a copy of the document.

**A plan is taken only if nothing changed meanwhile.** If the session's revision moved on since
the turn (the person edited while the thread was planning), the plan's changes are not applied:
they would undo that edit. The next turn plans again from the newer copy. The plan's assets are
always imported. Its access is given only with a plan that is taken (or one with no changes), so a
joiner can't edit before it holds the shared project and lose what it made.

The cost of this: while someone edits without pause, every plan is stale, and the others'
changes wait until they stop. A frame is about 16 ms and an edit lands at most once per frame, so
in practice the pause between drags and keystrokes is enough. Merging the plan into an edited
document instead would mean rebasing changesets, which the one mutation path doesn't do.

**A joiner takes the project in whole.** Applying a large project as changesets is slow on any
thread that holds the session: each changeset copies and checks the whole growing document
(`Changeset::stage_certified`), so 3000 cards took about 200 ms in a debug build. While the
session still holds the empty start a joiner opens (`collab::joining`, no history), the plan
carries the shared project itself, already validated on the thread and with the assets its cards
name (`collab::SharedProject`). `Session::adopt_shared` checks the assets are there and replaces
the document in one move, published as `share.adopt` like `adopt_joined`. It gives the window a
new document lineage, which the window adopts rather than taking it for another project opened
there.

The perf test (`a_large_project_comes_in_without_a_frame_over_budget`) shares 3000 cards and
times every frame of a joiner's catch-up against a 25 ms budget. The slowest frame took about
2 ms in a debug build; the same catch-up as changesets in one frame took about 200 ms.

**Still on the window:** a later large change from someone else is applied as changesets, at the
cost of an edit of that size. So is copying the document for a turn after each local edit (well
under a millisecond for 3000 cards). The browser build has no transport yet; when it gets one,
the thread's work splits across frames instead.
