## 2026-10-22 · Soul IDs are trained on a Train style card, and a lost creation is found by a tag in its name (#680)

- **Training is a trainer model, not a new card.** `higgsfield.soul-id-trainer` is a catalog model
  of the existing Train style card (`generation.style`): its wired pictures are the dataset and its
  `name` is the identity's name. The result is a style descriptor (provider `higgsfield`, family
  `soul-2`, weights naming `https://api.higgsfield.ai/v1/custom-references/{id}`), so the trained
  identity is kept, versioned and saved like any other trained style. SOUL 2 doesn't take a wired
  style input yet; the Soul ID is picked from the refreshed `higgsfield.soul_ids` list.
- **The price is Higgsfield's list price.** `POST /v1/custom-references` is listed at 2.50 USD a
  request (checked 2026-10-22), so the trainer is priced `per_call` and the run is quoted before
  anything is sent. The credit estimate endpoint is not asked about training.
- **A lost reply is resolved by listing, never by sending again.** Higgsfield documents no
  idempotency key for creation. The name sent ends in a tag derived from the run's idempotency
  key (`Mara [nc-<10 hex>]`), and every submit, a retry included, first lists the account for
  that tag and adopts a match without a POST. A reply that is lost, or doesn't read after the
  POST, is a transient failure the engine retries under the same key, so the listing resolves
  it. The tag protects retries within a run: a new run has a new key, and a submit that was in
  flight at a restart is not resumed by the journal at all. If this process sent the POST and the
  listing doesn't show it yet, the job fails rather than risk a second charge, and tells the
  person to look for the identity before training again. Only a 400, 401, 403 or 422 that
  Higgsfield explains forgets the POST so the run may send it again; the queue API's 423 and 503
  refusals are not assumed to hold for creation. The echoed name is compared by its tag only,
  since Higgsfield may tidy it. The tag is stripped from picker labels.
- **`model_version: "v2"` is sent** as the issue asks and SOUL 2's Soul ID check requires, though
  the open.higgsfield.ai page doesn't list the field.
- **No cancel.** Higgsfield documents no way to stop a training; cancelling is refused, and the
  identity can be deleted on Higgsfield's side.
- **The picker follows training.** When the desktop app records a Soul ID training it asks for
  `options.refresh`, so the new identity can be picked once it is ready. Pending and failed
  identities are shown as the card's run state and failure reason, not in the picker.
