## 2026-10-19 · Best-effort incidental maintenance during workspace construction (#445)

An unrelated crashed workspace must not take away native save/open or kept-journal replay when
its qualified payload unlink or retirement temporarily fails. Constructors stop their automatic
pass at the first classified old-candidate maintenance failure, preserve successful partial
removal counts, count one retained failure and then create an independent workspace under the
same held parent admission. Stopping instead of continuing preserves all existing work bounds:
4,096 parent entries, 64 inventory attempts, 256 payload unlinks and 512 KiB inventory backing.
`RetainedFailure` describes this incomplete pass; it is not a completed scan or eventual cleanup.

Failures are typed at their boundary. Old-candidate marker/inventory I/O, changed payloads and
payload/marker/directory retirement can be retained. Parent admission/identity, parent scan
integrity, cancellation, path/marker-name/buffer/allocation invariants and every new workspace
creation/marker/lock/write/sync/identity failure remain errors. An unproved candidate remains
unknown and cannot gain deletion authority. No ErrorKind-wide catch, retry, recursion or fallback
authority is introduced. Explicit public sweeps and named maintenance preserve their original
actionable errors; this policy applies only to automatic constructor maintenance.

Available valid markers and remaining payloads stay for later named reclamation. If marker unlink
succeeded and final-directory removal failed, the unmarked residue stays unproved; never recreate
a marker or adopt it automatically. Live owners, readers/captures, unknown files, symlinks/reparse
points and flat-inventory limits retain the existing refusal and lifetime contracts. The private
namespace remains cooperative, without a new hostile path-replacement guarantee.

Deterministic tests inject owned-root operation refusals per thread (not environment or OS
permissions), retain the genuine before-policy constructor failure, and cover partial progress,
payload type replacement, marker/final-directory residue, exact error/report controls, real
native save/open and kept-journal replay. Actual Windows sharing coverage remains Windows-only;
injected errors are not claimed as a physical Windows run. No wire, project or command change.
