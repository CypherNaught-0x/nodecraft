# Generate conversion admission and future host feedback

`nodecraft_engine::generate::provider_job` and `produced` accept standalone caller-owned jobs
and results. A document's per-value validation does not certify a whole request or provider result.
Runtime conversion remains authoritative; refusal leaves the original job/result with its caller.

## Runtime contract

Request admission checks depth 32, 4,096 aggregate visits (including empty containers), 4,096
flattened inputs, and 1 MiB aggregate text per original value. All parameters, input roots and
repeated flattened roles share a 64 MiB conservative representation/work budget. Strings cost
6 * UTF8 bytes + 34; values 128; input roots/emitted leaves 64 plus their charged role. Provider, model,
parameter names and idempotency key count. Media inputs need a `MediaSource` (#502): each costs
512 plus its MIME, and their stored sizes are summed against 64 MiB before any bytes are read. These logical bounds
are not exact JSON sizes, allocator backing or RSS limits.

Results retain the 4,096 item/route ceiling, 1 MiB aggregate routed text, 256 MiB binary item/raw
aggregate ceiling and 8 MiB converted representation/work budget. Existing route/record/string/media
charges remain. A private borrowed classification proof additionally costs 64 logical bytes per
result plus 32 for its root, identically on native and wasm. Compile-time checks bound the private
Rust representations by those constants. Near-budget results previously admitted can now be
refused for this extra scratch charge. Reservation is fallible. Rust may legally reserve more capacity than requested, so capacity equality
is not an admission condition. The proof charge bounds logical records/work, not allocator capacity,
rounding or resident memory. Bounded admission scratch may
allocate before late semantic failure; owned output strings, asset hashing/Arc clones and output
records wait until every result is admitted. Immutable borrowed text is UTF8-validated once.

After the bounded media sniffer identifies text/plain and claimed-type/UTF16 checks succeed, raw
text bytes must fit `routed_text_limit+3` before full UTF8 validation. The 3-byte allowance preserves
one standard UTF8 BOM followed by an exact-limit payload. It applies to every text/plain raw
buffer: a no-BOM input up to limit+3 may still be scanned and then refused by the unchanged trimmed
text limit. All leading BOMs are still removed within the raw bound. Repeated BOM padding above
the raw bound is now refused even when trimming would yield small text: an intentional work-bound
compatibility tightening. A separate cumulative budget charges every raw text-byte occurrence before UTF8 validation:
`routed_text_limit + 3 * actual_result_item_count`, with checked arithmetic. Repeated BOM bytes and
repeated occurrences of the same Arc each count. The configured item ceiling does not provide unused
allowance; actual items do. Direct Output::Text and binary media require no UTF8 scan and are not
charged to this budget, although each contributes the same bounded three-byte occurrence allowance.
They retain their raw, routed-text and representation checks. Several individually valid BOM-padded
items may therefore be refused together even when trimming would yield little or no text. Refusal
can follow earlier admitted scans but precedes the offending scan and every owned conversion.
The counter describes admitted scan bytes, not exact decoder instructions or RSS. Media sniffing
remains a separate bounded-prefix pass for each item. Late invalid UTF8 within the remaining scan
budget still fails before any conversion; over-budget invalid tails are refused before that scan.
Missing-route diagnostics name only the closed modality, never provider-controlled payloads.

## Planned pre-run and edit-time feedback (not implemented here)

A node can have several individually valid parameters/inputs whose combined visits, repeated
roles or bytes exceed request admission. A future host should show this before spending money,
without replacing runtime enforcement or copying a whole project to compute a warning.

The shared contract should be a borrowed validation/summary entry point over the SAME request
admission implementation used by `provider_job`, not duplicated UI constants or a second traversal
with different counting rules. It should accept the actual immutable standalone `JobSpec` and
report controlled reasons plus aggregate visits, flattened input count, logical byte charges and
their limits. It must not construct an owned provider request just to report feedback. Its summary
is advisory data, not an execution capability. This document does not introduce that public API.

At edit time the host may evaluate a materialized spec for the current node/revision and display
an actionable aggregate warning. Deferred or upstream inputs may be unavailable; report that
uncertainty rather than certifying the future run. At dispatch the host must resolve current inputs
and call runtime admission again, even after an earlier advisory success. Edits, changed upstream
results, repeated roles and different materialization can invalidate an earlier summary. The host
retains the actual spec/result on failure and must not reserve/call a provider after refusal.

The separate [host-feedback implementation (#307)](https://github.com/CypherNaught-0x/nodecraft/issues/307)
should test:

- individually valid values whose combined empty-container visits exceed 4,096;
- repeated flattened roles and metadata driving the representation budget;
- advisory/runtime parity for the same immutable spec, including exact/one-over boundaries;
- changed revisions/upstream materialization invalidating advisory feedback;
- unresolved inputs remaining explicitly provisional; and
- no proportional conversion or provider call when feedback/runtime admission refuses.

This is issue299's feedback-planning deliverable. No edit-time UI, desktop run host, new execution
authority, descriptor contract, paid accounting or persistence certification is shipped here.
