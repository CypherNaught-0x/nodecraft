## 2026-10-18 · Optional OpenAI image usage cannot discard valid outputs (#314)

Token usage is informational, not a monetary invoice. A lexically admitted complete usage subtree
is borrowed independently from the image reply. Invalid types, incomplete details, overflow or
inconsistent totals discard the entire usage record while validated images survive. Parsing a
complete separate subtree avoids accepting a partially consumed deserializer. Structural JSON,
resource limits and all image validation remain hard errors.

RateLimited/Busy and Retry-After do not certify nonacceptance or nonbilling. Keep uncertain
no-handle submissions terminal under the captured retry policy. Timeout and browser limitations
remain explicit in [Providers](../providers.md). No new replay, pricing or delivery proof is added.
