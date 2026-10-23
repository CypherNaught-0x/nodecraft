## 2026-10-23 · A timeline stores up to 128 clips and renders up to 32 (#585)

The [first Timeline record](2026-10-21-timeline.md) capped a timeline at 32 clips. That made a
Timeline with more than 32 wired takes uneditable: every edit stores the automatic clip of each
wire, so the stored list passed the cap before any clip was disabled. There are now two bounds.

- **Rendering: at most 32 enabled, wired clips** (`MAX_CLIPS`). A run still checks this before it
  imports anything. Commands refuse only a change that renders more than the node does now and more
  than 32: enabling a clip (`timeline.update`), splitting an enabled wired one (`timeline.split`),
  or a `node.timeline_set` that renders more than the stored timeline did. So disabling clips, and
  going back to automatic clips on a node that already plays every wire, are always accepted.
  Disabled and disconnected clips cost nothing to render.
- **Storage: at most 128 clips** (`MAX_STRUCTURAL_CLIPS`, four times the render bound), counting
  the automatic clips an edit stores. Four times leaves room to keep several alternative takes per
  rendered slot (40+ wired takes, plus old ones whose wires are gone) while keeping the stored
  JSON, the per-edit clone and the editor's lists small and bounded. The editor also refuses more
  than 128 wired inputs instead of dropping some, which would lose their clips on the next edit.
- **Recovering from over the bound.** A timeline over 128 is only reachable by wiring new media
  into one that already stores many clips, or by opening a project saved before this bound.
  `timeline.remove` then deletes from the stored clips alone, leaving the automatic ones automatic,
  so deleting old takes one by one brings it back within the bound. A stored list that is itself
  over the bound can't be read; the error says to replace it with `node.timeline_set`, which
  accepts an unreadable stored timeline. Such a file is kept as it is until then.
