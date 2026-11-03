## 2026-11-04 · A trained Soul ID stays a style descriptor, marked as a provider-bound handle (#180)

#180 asked for "a separate typed provider-identity resource". What landed in #681 and #685 keeps a
trained Soul ID as a style descriptor instead: provider `higgsfield`, trainer
`higgsfield/soul-id/v2`, family `soul-2`, weights naming
`https://api.higgsfield.ai/v1/custom-references/{id}`
([2026-10-22-soul-id-training.md](2026-10-22-soul-id-training.md)). The repository owner accepted
that design on 2026-11-04. This record says why it is acceptable, what makes it safe, and what
would change it.

**Why it is acceptable.**

- **Family checks keep it away from LoRA models.** A style goes into a card only through
  `style.use`/`style.card`, and only when the model's catalog `style_family` is the style's
  family. No model declares `soul-2`, so a Soul ID never reaches a FLUX LoRA's style input and a
  LoRA never reaches SOUL 2. The fal adapter also refuses any descriptor that isn't its own FLUX
  trainer's. Nothing converts an identity into a LoRA or substitutes raw images for it.
- **The handle can be told apart.** `nodecraft_engine::soul_ids` decides it from the asset's type
  (family `soul-2`), so it holds even when the descriptor's bytes are missing.
  `nodecraft_providers::higgsfield::soul_id` reads the id only from a descriptor the trainer
  wrote: Higgsfield's provider, the Soul ID trainer, the family and the documented reference URL
  of a UUID. Anything else names no id.
- **It reuses what a style already has.** Versioned history, saving, the Library's Styles tab,
  the training card's run states and failure reasons, and the separation of the identity id from
  generation request ids all come with the style. A second resource type would duplicate them.

**What marks it as provider-bound (this change).**

- `library.styles` reports `portable: false`, the `soul_id`, and `available`. The last is
  `listed`, `unlisted` or `unchecked` against the account whose Soul IDs were last fetched, plus
  an `account_bound` count for the project. The Styles tab says the identity is bound to the
  account that trained it, and shows "Unavailable on this account" when the fetched list doesn't
  have it.
- `history.export` and the folder exports (`export.local_plan`/`run`, Export to folder cards)
  refuse a Soul ID with a reason. It is never written out as if it were a portable file. My
  library and `library.attach` already refuse styles.
- The Share live panel says the project uses Soul IDs that work only on the training account. A
  shared or reopened project keeps the handle and every card setting. Nothing is cleared.
- On another account, a fetched list blocks the run as "not one of the available options". With
  no list (no key, offline, not refreshed), the adapter's free `GET /v1/custom-references/{id}`
  refuses an unknown, pending or failed identity before the paid submit. Both refusals keep the
  setting.
- **Use on selected card** on a SOUL 2 card sets the card's `custom_reference_id`, the same value
  the picker sets, through `node.param_set`'s checks, in one undo step. It is refused when the
  fetched list lacks the id. SOUL 2 gets no style input: its documented request names the
  identity as a parameter, so a wire would be a second way to say the same thing.

**What would change this decision.**

- A second backend with account-bound identities. A per-family check would no longer be enough,
  and the descriptor would need an explicit binding field (a new `nodecraft_style` version) or a
  separate resource type.
- A model that takes a Soul ID as a wired input, or SOUL 2 image-to-image with scene images (a
  follow-up issue; its request shape is unverified). If the identity has to travel with other
  inputs, a typed input may fit better than a parameter.
- A need to rename, delete or move identities between accounts from NodeCraft. Higgsfield
  documents none of these.
- Higgsfield documenting the list reply, a creation idempotency key, or `model_version` on
  creation. These would relax the strict parser and the tag-and-list recovery, not the
  representation.
