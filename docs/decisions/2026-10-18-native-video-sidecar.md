## 2026-10-18 · Explicit external video sidecar (#107)

- **Native video uses an explicit external tool service (#107).** The optional standalone media
  feature owns bounded probe/decode/scrub/render processes and private scratch files. It never
  downloads or redistributes FFmpeg, runs shell commands, or treats codec flags as an OS sandbox.
  Caller-provided RGBA is composited against an explicit opaque background in linear sRGB for
  VP9 WebM. Color assumptions are typed and reported; missing tags are not silently treated as
  accurate metadata. Shared quotas retain externally pinned artifacts; final-pin cleanup belongs
  to the existing supervisor, not synchronous Drop. See `docs/media-video.md`.

The native filesystem/process/thread adapter lives privately inside standalone L0 media because media must have no workspace dependencies. It is cfg-excluded on wasm, invoked by the bounded worker service, and is the platform boundary for this feature. This deliberate local adapter does not authorize process spawning from graph, nodes, engine handlers or UI frames.
