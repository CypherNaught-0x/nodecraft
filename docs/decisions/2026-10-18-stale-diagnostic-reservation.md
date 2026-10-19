## 2026-10-18 · Reserve stale diagnostic output before hooks (Part of #339)

Detailed stale reports reserve 576 bytes from the existing 256 MiB query budget before routing
initialization or recipe hooks. A private credit in that hasher pays for one returned diagnostic;
formatting consumes it once without charging again. The reservation remains charged even when
no unknown result is found. Admission refusal remains sticky and returns an error, never a partial
report. A hash that exceeds the remaining budget still fails.

Legacy stale-set queries and standalone input-hash diagnostics retain their existing budgets and
hash bytes. `unknown_total` counts checked active results with recorded hashes whose current hash
is unavailable, including several consumers of one rejected upstream; it does not count unique
faulty nodes. This is one follow-up, not completion of #339.
