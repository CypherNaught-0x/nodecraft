## 2026-10-28 · MiniMax video, voice and music on the existing fal queue (#27)

- Cover all three issue families together: current H3 text/first-frame video, Speech 2.8 HD
  and Music 3. Reuse Fal credentials, uncertainty/resume/cancel and strict origin checks.
  Canonical roots are `minimax/h3`, `fal-ai/minimax` and `minimax/music-3`, while submissions
  retain their full aliases. No native MiniMax API or older Music2 wire fields are invented.
- Explicit 480P/768P video and integer duration bind the budget to current $0.05/$0.06 per
  second rates. Disable expansion; omit premium resolutions and optional driving audio.
  Speech fixes URL/MP3/32kHz/mono, quotes full UTF-8 bytes conservatively at $0.10/1000 and
  rejects long pause markup. Music fixes 44.1kHz16-bit stereo WAV and defaults30steps/guidance1.7,
  quoted at $0.002 per requested maximum second. Prices are dated estimates, not invoices.
- Keep lyrics in the existing generated model parameter UI because connected non-prompt text
  roles are not supported by generation cards. Refuse section tags with same-line words
  instead of silently losing user lyrics. No new graph mutation path or UI-only execution.
- Save each new model's admitted duration bound with its queue handle; legacy handle shapes
  remain unchanged. New containers receive whole-extent, codec-setting and duration admission,
  and result metadata must agree. Scoped individual/aggregate caps are256MiB; other Fal models
  retain their previous caps. MP3 validation is structural, not full sample decoding.
- All four models require native streaming assets before paid work, resolving the executed
  catalog identity even for custom IDs and deferred Techniques. Reuse the #37 worker Seal
  pipeline, including held persistence; do not create another storage path. Browser creates
  refuse locally. Author original fixtures and exercise native retention/save/reopen; live
  provider quality remains opt-in and unmeasured.
- Primary [H3](https://fal.ai/models/minimax/h3/text-to-video/api),
  [speech](https://fal.ai/models/fal-ai/minimax/speech-2.8-hd/api),
  [music](https://fal.ai/models/minimax/music-3/api) and
  [queue](https://fal.ai/docs/documentation/model-apis/inference/queue) contracts checked2026-10-28.
