# 2026-10-17 · Model calls as jobs, the mock provider and golden graphs (#79)

- **`JobSpec::Generate` is the one way a node calls a model.** It names a provider adapter by id,
  a model, the node's parameters and role-tagged inputs, plus a routing table of output ports by
  modality. The provider id is also the scheduler lane, so per-provider limits apply. `exec::run`
  treats it like any job; `SourceRunner` refuses it. `JobSpec` lives only in memory today; before
  it is persisted (resuming runs after a restart), later fields need serde defaults.
- **`engine::generate` translates in both directions, and treats results as hostile input.**
  - Out: parameters go to the provider as plain JSON, and non-finite numbers are refused. Inputs
    go as role-tagged text, with a list sending one entry per item, up to the list limit. Media
    inputs are refused as unsupported until the asset store exists.
  - Back: inline bytes are sniffed, and the provider's claimed type is ignored. Bytes that
    sniff as text count only if the provider claimed text, so an error page posing as an image
    fails the job; UTF-16 text is refused. Each result goes to the first output port of its
    modality. A result no port takes, or a call that returned nothing, fails the job. Text comes back as a text value. Media comes back as a value that references the bytes by
    Blake3 hash, and the host stores the bytes. URL outputs must be downloaded first, and inline
    outputs are capped at 256 MiB.
- **`nodecraft-testkit` provides `MockProvider`**, a deterministic fake for text, image, video and
  audio models. Its results depend only on model, parameters and inputs. Media results carry real
  format signatures, so hosts sniff them like real output.
  - It can be scripted per model: polls before done, a reported cost, and failure at submit, with
    a lost reply (billed, then the reply errors), during polls, or of the job itself.
  - It records requests and bills once per idempotency key. A retry gets the original job or
    result back, and cancelled jobs stay cancelled.
  - It also provides mock generation kinds (`mock.text`, `mock.image`).
- **Golden graphs run a canvas end to end:** plan, run executor, `run_to_completion` against the
  mock, results recorded with `record_changes`. They don't exercise the scheduler or driver.
  - Three blessed JSON files per graph: the step report (outputs, or the failure reason, plus
    every provider request), the project, and the project with its results recorded. The
    committed project file must load back to the same project.
  - Each golden checks that nothing is stale after its results are recorded.
  - Bless with `NODECRAFT_BLESS=1`. Golden files are LF-only (`.gitattributes`), and comparison
    ignores CR. The project files are graph JSON until the `.ncraft` archive (#134) lands.
