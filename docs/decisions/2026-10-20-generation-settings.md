## 2026-10-20 · Each result records the settings its provider jobs were sent (#162)

- **Per job, as sent.** `Generation.settings` holds one `JobSettings` per provider job, in job
  order: provider, provider instance, the provider's model id and the parameters after catalog
  admission canonicalised them, so it is what the provider actually received. A fanned result
  (#92) tags each job with the index of its item. Local jobs (sources, actions) record nothing;
  their inputs are already the result's input hash.
- **Not identity.** Settings describe a result; staleness and caching still compare input hashes.
  A cache copy keeps the settings of the result it copies, since that is how its outputs were
  made.
- **Never at the cost of a result.** A result whose settings would push it over the node byte
  limit is recorded without them, as an over-budget input hash is recorded as unknown.
- **Format.** The field is omitted when empty, so older files are unchanged, and version 1
  refuses it like input hashes and item tags.
- **Random seeds stay refused** for now. Admission requires a job's parameters to equal the
  source's submission exactly, so a drawn seed has to be added after admission, by the run that
  owns the entropy; that is a separate change, and its seeds will be recorded here.
