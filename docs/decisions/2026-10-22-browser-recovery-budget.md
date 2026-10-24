## 2026-10-22 · Browser recovery storage budget (#726)

Recovery checkpoint, frame and hold payloads share a 256 MiB budget across the origin's slots.
This is a retention budget, not an origin quota guarantee: open tabs and the recovery copy
offered or deferred by this page keep their Web Locks and may exceed it. Browser storage overhead,
provider keys and other site data do not count toward the payload budget.

A cursor inventories every stored record, one value at a time, without copying all journals
into Rust. The existing one-million-record bound refuses incomplete inventories rather than
silently treating an incomplete total as safe. Exceeding that safety bound disables recovery
for the page rather than writing against an unknown total; save work to a file before clearing
the site's recovery database to repair such an oversized inventory. Byte accounting uses u64 so inventories above
4 GiB stay exact on wasm32, including after pruning. Hold-marker replacement subtracts the
old marker's payload before charging its replacement, including damaged or repeated markers.
At startup, the newest eight slots remain
the offer window. Cleanup also checks checkpoint-only and checkpointless slots outside that window,
removing slots with no recoverable work in the same transaction as budget cleanup. Other older
work stays unless the byte budget requires its removal.

Budget cleanup removes the oldest available copies first and reports the successful removal
count. A temporary Web Lock must be acquired before deletion and is held through the transaction.
Temporary scan/pruning claims are released on success, failure and cancellation; only this tab's
slot and its offered recovery remain claimed, including after Keep/Later or a failed Recover.
Claiming an already held slot refuses, even when this shelf owns it: only a newly acquired claim
grants temporary deletion/release authority. Cleanup never takes an active tab's copy.

The complete startup inventory is reused for small writes. When projected payload bytes exceed
the budget, storage is inventoried again before pruning. After any failed write the cache is
lost, so the retry also inventories storage again. Independently, a scheduled frame refreshes
the inventory every five minutes, even without edits: other tabs can grow and then close after
our initial snapshot. Closed-tab over-budget work is therefore pruned at the next interval
(or the next frame when a browser throttles background pages), without waiting for an origin
quota error or this tab's own growth. This intentionally longer idle interval limits IndexedDB
payload structured-cloning and I/O; each cursor value is inspected to count its byte length.
Own writes that project over budget still refresh immediately. Pruning and the queued journal writes share
one atomic transaction: a failed replacement leaves the old recovery copies intact, and no
removal notice is published for a failed batch. Active tabs may grow concurrently beyond the
budget; their recovery remains protected, and periodic maintenance can prune them after they close.
