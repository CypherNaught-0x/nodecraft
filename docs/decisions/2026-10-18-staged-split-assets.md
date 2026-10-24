## 2026-10-18 · Split assets publish through one candidate (#342)

The landed split helper now shares the Session-bound prepared-import authority used by file drops.
All tile bytes, nodes, params and returned IDs are prepared before one paste Edit; repeated public
imports and global pruning are not rollback. Refusal preserves unrelated staged bytes, old receipts
and reader pins. Non-logical maintenance can report retry guidance while preserving the original
error. An ambiguous native append may require recovery repair without publishing logical state.
This supersedes the original split decision's global-pruning implementation description.

Admission charges only new unique hashes with exact lengths against the context's retained count
and resident/native byte limits. At most 64 tile descriptors share one encoded Arc per identity;
completed unique encoded inputs remain capped at 48 MiB per operation, and each accepted tile at
16 MiB. These are separate from the native 64 GiB project limit. Arc conversion, resident candidate
bodies, read/copy temporaries and unchanged pixel/codec ownership are separately accounted phases,
not a total-RSS claim. Existing corrupt indexed duplicates must be verified, never repaired from
newly encoded good input.

A selected native image uses a verified reader, with descriptor length capped before reading or
allocation by the existing 256 MiB decoder-input limit. A private fallible buffer reads at most
64 KiB per call, checks exact length/EOF and bounds Interrupted retries to 1,024 total. It ends after
unchanged decoding and before tile encoding. This bounded consumer does not raise read_small's
16 MiB cap or make native storage/Open eager. All media/format APIs and physical-harness inputs
remain unchanged. No wall-clock I/O, constant memory, whole media validity or new browser streaming
host is promised. Source selection, uneven row-major coverage, layout, UI and one undo step remain.
