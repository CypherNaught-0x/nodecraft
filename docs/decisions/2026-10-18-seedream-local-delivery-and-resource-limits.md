## 2026-10-18 · Seedream local delivery evidence and resource limits (#15)

Seedream arms the existing audited invocation-local delivery scope before constructing its
submit future. Borrowed validation, header probes, complete reference decoding and bounded
request encoding finish while it is still clean. It marks the scope unknown before invoking
send_no_redirect, including before a transport can construct an effectful future. Only local
refusals supply nonacceptance; no error kind, HTTP response or cancellation implies nonbilling.
The legacy raw submit path remains available without manufacturing caller evidence.

The supported reference ceiling is fourteen ordered occurrences including duplicates, subject to
8 MiB each, 48 MiB aggregate, 16,777,216 pixels each and 32,000,000 aggregate pixels. Inputs are
borrowed; no deduplication, implicit resize or quota increase hides these bounds. Full validation
remains necessary: a header alone does not prove valid media. Watermark is fixed on.

The original logical phase worksheet is retained: 129 MiB for decoded/rotation plus metadata
validation, 76 MiB for encoding, and 222 MiB conservative response/request/raw/Arc-copy/pixel
owners. References are sequentially decoded and pixels drop before request encoding; base64
scratch is one reference at a time. These figures exclude caller buffers, private codec
workspace/copies, transport buffers, allocator overhead and concurrent operations. They are
logical named-owner limits, not total allocation or RSS measurements. Media max_alloc preflights
decoded pixels plus orientation; most codecs do not treat it as a universal workspace ceiling.
Moving this worksheet from provider setup documentation adds no new measured memory evidence.

Paid output keeps the reviewed strict contract: exact dated model, required size agreeing with
actual JPEG dimensions, single full JPEG and no trailing bytes. We do not infer undocumented
model aliases or discard a tail to preserve a decoder prefix. A schema change can therefore
cause a post-send possibly billed refusal; bad optional usage alone preserves a valid image.
The required-size policy is superseded by
[paid image output admission (#421)](2026-10-22-paid-image-output-admission.md): the JPEG itself
is authoritative, while dated model and complete framing checks remain strict.
See [provider setup and limits](../providers.md#seedream-bounds-and-billing).
