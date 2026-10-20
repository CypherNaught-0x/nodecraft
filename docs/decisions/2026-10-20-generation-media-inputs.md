## 2026-10-20 · Generation cards send wired media from the asset store (#502)

- **The mode declares the roles.** A generation card has one input port per role its model's
  modes declare. Wired media is planned only onto those roles: each asset must carry the role's
  modality (`Modality::accepts_mime`), and each role gets between its `min` and `max` assets.
  The planner folds all of a port's wires into one list, so the mode is chosen by counting the
  assets on each media port, not the wires. The job lists the prompt first, then roles in the mode's
  declared order, each role's assets in wire order. A wire to a role the model doesn't declare,
  or of another modality, is refused while planning, so the quote blocks and nothing is paid.
- **Connected text feeds only the prompt.** A text role other than the prompt has no source yet;
  wiring it is refused, and a mode that requires one stays unsupported.
- **Planning stays pure; the engine reads bytes.** Plans carry `Value::Media` asset references.
  `generate::provider_job_with_media` and `AuthorizedRun::queued_job_with_media` turn them into
  `JobInput::Media` through a `MediaSource`, normally the session's `AssetStore`, which reads
  verified bytes. `provider_job` and `queued_job` without a source still refuse media.
- **Size is checked before reading.** One model call sends at most 64 MiB of media
  (`MAX_MEDIA_INPUT_BYTES`). Admission sums the stored sizes from metadata first, so an
  oversized call reads nothing; the bytes actually read are held to the same cap. The read is
  synchronous and holds the media in memory (briefly twice, as it moves into the job), so the
  host calls it off the UI thread.
- **Checked at submission, not at the quote.** The quote doesn't look the assets up, so a missing
  or oversized asset fails its job when the host builds it, before the provider is called and
  before anything is charged. A quote-time check belongs with the run host (#409).
- **Adapters refuse what they can't send.** The chat adapter takes images only; other media is
  refused with an unsupported-input error, not sent labelled as an image.
- **Not yet.** The desktop run host that calls `queued_job_with_media` is #409. Streaming large
  media to providers, and uploads of assets bigger than the cap, are later work.
