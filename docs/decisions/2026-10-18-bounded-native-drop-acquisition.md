## 2026-10-18 · Bound native drop acquisition to one worker (#335)

Read dropped native paths on one process-wide worker, never on a canvas frame. Validate metadata
on the actual opened File, then read and close that same handle. A symlink to a regular file is
accepted; this is not a no-follow or filesystem snapshot guarantee. On Unix the open passes
O_NONBLOCK (via `libc`), so a FIFO opens at once and the regular-file check refuses it; other
special files and slow filesystems can still block the worker. Safe portable std cannot
interrupt a blocked open, metadata, read or close. One occupied lease spans bounded preparation,
queued/running I/O, result ownership and synchronous engine delivery. Cancel, timeout and app loss
abandon delivery without waiting, but do not release a blocked worker's lease or spawn a
replacement. Later attempts report busy until it returns, or the user restarts. Startup failure
is cached. Ordinary caught request panics leave the same worker available.

Inspect only the first 32 borrowed dropped handles; retain no original handle or per-tail skip
list. Accepted path capacity is at most 32 KiB in native encoding, basename capacity at most
320 UTF-8 bytes/80 scalars. Reject longer paths instead of opening a truncated path. Raw body
Vec capacities, including the current partial file, total at most 48 MiB; a file is at most
16 MiB and each read request at most 64 KiB. A separate 48 MiB returned-body work allowance is
not refunded after failed files. At most 32 one-byte EOF probes and 1024 allowed Interrupted
responses are tolerated across the batch; at most one further read detects refusal. After that
Interrupted outcome the batch is exhausted: no later open, body read or EOF probe runs. Codec/store,
old document/history, OS input, allocator and thread/kernel overhead are separate owners. These
bounds are not a total-RSS or physical-device-I/O claim.

One absolute ten-second monotonic deadline covers admission through accepted delivery, with
checks around backend calls and result handoff. Pending views request a repaint after 50 ms;
expiry/cancellation is observed on the next executing frame, not a real-time scheduling promise.
The pending control owns its initial press through release and keeps Cancel present if I/O
finishes during that gesture. Existing text, popup and canvas gesture owners retain their input.
No Escape shortcut is added.

Capture the private Geometry Session identity Arc, project, canvas, revision and placement.
Reject replacements even with equal ids/revision; document edits/Open or canvas switches cancel.
View-only pan/zoom/selection preserve the original placement. Save without a document revision
change may accept a result only by freshly invoking the current engine import once. Worker code
never owns Session authority or a mutation callback. Accepted nodes still use engine sniffing,
command admission, selection and one undo step. Synchronous decode/hash/journal/commit after
acceptance remains synchronous; browser acquisition and larger asset policy are separate work.
