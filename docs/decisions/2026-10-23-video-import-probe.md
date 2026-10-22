## 2026-10-23 · Video metadata before import publication (#572)

- **Header facts stay the default.** MP4, MOV and WebM/Matroska already carry bounded duration
  and geometry probes. Imports with complete headers use them without external tools, including
  browsers and headless callers. Unsupported formats are not enabled by installing ffprobe.
- **Native file frontends may fill gaps before their edit.** Canvas drops and App mode file
  pickers reuse the app's configured video service when header fields are unknown. The existing
  service owns staging, bounded workers, deadlines and restricted subprocess protocols; no tool
  runs in an engine command or blocks a UI frame. The app starts at most one import probe at a
  time per drop, with a three-second aggregate best-effort wait and a one-second reserve inside its
  existing ten-second acquisition/publication deadline. Form probes also wait at most three
  seconds. Cancellation, document changes and closed forms drop the probe job.
- **Failure does not refuse otherwise valid media.** Missing tools, unsupported metadata,
  busy services and timeouts retain the old unknown fields and continue normal import. Known
  header facts win. The worker converts bounded native drop buffers into shared immutable bytes;
  conversion can temporarily retain one extra file (at most 16 MiB) beyond the 48 MiB batch.
- **Proofs are bound to bytes, not caller JSON.** The engine's optional `ProbedMovie` can be
  constructed only from a video service `VideoAsset`; its hash must match the actual import.
  Enrichment is part of the same candidate-store publication and undo step. Pure import APIs
  remain synchronous and process-free; native hosts without a video service (including current
  CLI multipart serving) keep header-only behavior. They may explicitly acquire a service proof
  before calling the enriched import API. Stored duration is then visible to `timeline.list`
  and other headless readers without playback or a render.
