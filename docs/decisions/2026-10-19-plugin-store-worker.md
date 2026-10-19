## 2026-10-19 · Plugin store worker, retained results and native locking (#431)

The desktop registers the plugin service without opening its folder or index on the egui
thread. A dedicated worker lazily opens the store and serializes its operations. Admission
returns `{pending: string}`. `plugins.result` reads a non-consuming envelope with `pending`,
`status` (`queued`, `running`, `completed`, `failed`) and, when terminal, `result` or `error`.
`plugins.forget` acknowledges only terminal results. Package operations remain human-only for
MCP; reading and acknowledging an opaque result are automation-safe. Synchronous hosts keep the
existing direct-result protocol.

The worker retains at most 16 slots in total: queued, active and completed. Request fields are
validated before queueing; at most 8 KiB of logical text belongs to each flat queued request,
including a location of at most 4096 UTF-8 bytes. No archive body is queued. A result must fit the
command JSON shape/text limits and 1 MiB of encoded JSON. A larger result becomes a retained
explicit error that warns the operation may already have committed; results are never silently
truncated or presented as rolled back. These are logical protocol limits, not allocator or RSS
bounds. Result reads copy only an admitted bounded value. Pending IDs contain a random worker
nonce and checked sequence and have no authority in another worker or after restart.

A caught backend panic terminates that worker: the active operation is reported as uncertain,
queued work is refused, earlier results stay available, and no operation is replayed. A normal
initialization error is a retained failure; a later explicit request can try initialization
again. Dropping the host abandons its volatile results and closes admission, without joining a
possibly blocked filesystem call on the UI thread. There is no hard deadline or cancellation
promise for an OS call already in progress, and no durable operation-result journal.

Every public native store operation holds a separately opened, nonblocking OS file lock on the
permanent `.lock` inode. It covers startup sweep, archive verification and writes, the current
index check, atomic publication, and cleanup. Another process/window receives Busy while the
lock is held. Every mutator compares its cached index under that guard before making incoming-archive or
no-op decisions, reading/writing package objects or publishing an index. A stale writer reloads
and returns Conflict before the requested side effects. The final publication check remains.
Startup temporary-file cleanup follows current-index decoding under the guard.
Memory backends use a no-op transaction guard. The lock is cooperative inside the app-owned
namespace; it is not a security boundary against an actor replacing its files. Never unlink,
replace or clone the gate handle while participating in this protocol.

Keep the existing `<settings>/plugins/store` and `<settings>/plugins/trust.json` paths for this
change. A later data-directory move needs a separate migration protocol for both package bytes
and trust decisions, including interrupted/cross-volume transfers, two live app versions and
which root is authoritative. Merely copying or preferring a new root could split installs or
revocations. No migration or new data-directory contract is shipped here.

The command panic corpus now installs a real memory store. Its test-only file-acquisition seam
redirects corpus locations to bounded private valid/malformed archives; it does not read arbitrary
user paths. Native file locking has separate same-process and child-process controls. No package
code is executed and no document, undo step or project wire format changes.
