## 2026-10-22 · Private canvases (#121)

A canvas can be marked **private** (`canvas.set_private`, the invertible change
`SetCanvasPrivate`). When the project is shared live ([share links](2026-10-22-share-links.md)),
a private canvas stays with the session that marked it. Nobody in the room sees it, whatever
their role.

**It never enters the shared document.** Filtering per member was the other option, and it
can't work. A yrs update and a `Step2` cover the whole document, and one canvas's parts are
spread over every shared map. The relay can't strip one canvas out of them. So `Collab` keeps a
private canvas out instead:

- It shares, publishes and diffs the project as the room sees it, without private canvases
  (`collab::shared`).
- Each settle puts the session's private canvases back as the session has them.
- Assets go out only if the room's project uses them. An `AssetGet` for a hash only a private
  canvas uses goes unanswered, even from an editor who somehow knows the hash.

**Making a shared canvas private takes it away from everyone else,** as deleting it would.
Making it shared again gives it back. Someone may change the canvas just as it is made private,
and the shared document can then still hold it. The next settle sees this and takes it out
again; the private copy is kept as it was.

**The flag is saved with the project.** A canvas stays private when the project is shared
again. Only a role that edits may change it. A commenter's or viewer's session is refused by its
access check, and they can't make canvases anyway. Agents may use `canvas.set_private`: it is an
ordinary, undoable edit.

**Not done here:** private canvases shared with chosen people rather than nobody. That would
need a separate document per canvas (yrs subdocuments) and a relay that routes by grant.
