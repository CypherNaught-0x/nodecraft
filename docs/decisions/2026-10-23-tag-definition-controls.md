## 2026-10-23 · Stable tag cues and deliberate definition edits (#412)

Tag lists show the complete persisted ID above their bounded title instead of a presentation ordinal. The ID remains unchanged when renaming reorders rows; no abbreviated-ID collision policy or extra persistent identifier is needed. Two-line rows remain virtualized. Matching cards receive dashed corners inset inside the card so selection retains its own border in both themes. Fixed-length corner strokes bound paint allocations independently of card perimeter.

Definition controls live in each filter row’s context menu. Rename edits a bounded draft, then dispatches existing `tag.put` while preserving color; delete confirms before existing `tag.remove`. Neither uses a second document mutation path. Session identity, canvas and original definition checks discard stale drafts rather than overwriting newer metadata. Cancellation never changes document history.

Deletion continues dropping the removed ID from transient filter choices. Undo restores metadata and memberships, while filtering remains view state and is not rewound. The delete confirmation states that the restored tag must be selected again. This policy retains no deleted IDs and keeps later filter changes independent of document history.
