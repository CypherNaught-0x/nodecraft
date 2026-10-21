## 2026-10-21 · A trained style is a card's result, not a project record (#167)

- **Training is a generation card.** A Train style card takes the dataset as wired images and
  runs a trainer model from the catalog like any other paid generation. The quote, budgets, the
  run journal, resuming after a restart, idempotency keys and "possibly billed" accounting are
  the ones every generation uses, so a retry after a lost reply resumes the provider's job and
  never pays for a second one. Queued, training, ready, failed and cancelled are the card's run
  states. Retraining adds a result to the card's history, and the active result is the style the
  card offers. This follows Elements (`2026-10-21-elements.md`), which are nodes for the same
  reasons. A separate `Style` record in the project was set aside: it would need its own job
  tracking, journal entries, charges without a node and file-format gating, all of which already
  exist for cards.
- **A style is a small descriptor, not the weights.** The result is a NodeCraft style
  descriptor (`application/vnd.nodecraft.style+json`, modality `style`): format version, the
  trainer model, the base-model family it was trained for, the trigger word, and where the
  provider keeps the weights. The weights stay with the provider; the project keeps only the
  descriptor, so a project never grows by a LoRA's size. A weights reference is an `https` URL
  with no user info, query or fragment, so a signed or keyed link is refused rather than saved.
  Whoever has the project has the link, the same as for any provider-hosted result.
- **Provenance is the card's.** The dataset is the images wired into the card, recorded by hash
  in the result's history like any generation's inputs; nothing copies them into parameters. The
  trainer, its settings and the run's provider job are in the same record.
- **Compatibility is data.** A catalog model names its `style_family`. A trainer (a mode whose
  output is `style`) trains for that family; an inference model with a `style` input role takes
  styles of that family. Running a card whose wired style was trained for another family fails
  with both names, before anything is sent. Strength and other LoRA settings are ordinary
  parameters of the inference model's schema.
- **Local LoRAs stay separate.** Choosing a LoRA installed in ComfyUI is a ComfyUI option list
  (#162). Neither path implies the other: a trained style feeds only models that declare a
  `style` role, and a ComfyUI LoRA name is never a style.
- **Training prices count steps.** Trainers are priced with the `thousand_steps` unit, counted
  from the trainer's `steps` setting, so the quote follows the steps asked for.
