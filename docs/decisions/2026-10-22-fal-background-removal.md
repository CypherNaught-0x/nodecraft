## 2026-10-22 · Image-only Bria background removal (#749)

**Context.** The Image Tool needs a catalog-backed background-removal mode. The fal adapter
uses a closed model enum so a catalog edit cannot dispatch arbitrary endpoints. Official
[Bria pricing](https://fal.ai/models/fal-ai/bria/background/remove) checked 2026-10-22 is
0.018 USD per image without a prompt and 0.03 USD with a prompt; placeholder compute prices
on other endpoints do not justify a zero-cost estimate.

**Decision.** Add `fal.bria-rmbg`, mode `remove-background`, with exactly one required image
role and an image output. Allowlist `fal-ai/bria/background/remove` in the adapter. Send only
`image_url` and `sync_mode=false`, following the [HTTP schema](https://fal.ai/models/fal-ai/bria/background/remove/api).
Refuse text roles and all parameters, including prompts, before codecs, encoding or HTTP;
this keeps the dated per-call estimate tied to the admitted request. Preserve the existing
1 MiB / 1024 px input limits and existing 16 MiB output cap. Record the probed input dimensions in the resumable
handle and refuse an output geometry the configured limits cannot admit before submission.
Accept only a singular, complete PNG with those dimensions, retaining its alpha channel.
Refuse images requiring an EXIF rotation or reflection before codecs, encoding or HTTP;
forwarding their original bytes would otherwise depend on the service's unverified EXIF policy.

Queue operation URLs use the explicit `fal-ai/bria` owner/application root, as required by the
[queue contract](https://fal.ai/docs/documentation/model-apis/inference/queue); the create URL
keeps the full endpoint. Existing origin, no-redirect, credential isolation, ambiguous-create
and best-effort cancellation policies stay in force. The generic conservative cancellation
status policy is outside this model addition.

**Consequences.** Original procedural PNG fixtures exercise request and response admission,
resume, cancellation, alpha preservation and hostile restored handles. An offline catalog →
generated card → quote/consent → shipping RunHost → real Fal adapter fixture proves that one
stored image reaches the wire and its distinct transparent output reaches generation history,
without a paid request. Live alias availability, live receipts and actual billing remain
unverified. The first opt-in live check must confirm exact input-size preservation without padding
or resizing. The dedicated Image Tool menu (#740) is independent and has not landed; no optional
image-to-layers mode or menu implementation is included here.
