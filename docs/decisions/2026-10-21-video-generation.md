## 2026-10-21 · Video generation is a generation kind whose modes say what they send (#559)

- **One more generation kind, not a new engine.** `generate.video` reuses the image/audio
  generation path: the model's catalog modes declare ordered input roles with counts, the
  wired roles choose the mode, and the job sends inputs in the model's role order rather than
  the wiring order. There is no universal frame limit or aspect list; both come from the model.
- **A mode lists the settings it sends.** `params` on a mode narrows the model's settings for
  that mode (a lip-sync clip follows its speech, so it sends no `duration`). Absent means every
  setting. The card hides the others while that mode is wired but keeps their values. Run
  admission accepts the narrower submission exactly, and only from a job whose inputs fit that
  mode, instead of re-adding defaults.
- **Audio use is declared, and long audio is refused before sending.** `audio` on a mode names
  its audio role, whether the clip follows the audio, the longest track, what happens to a
  longer one (`refuse`, the default, or `provider_trims`) and what the result's soundtrack is.
  A refused track stops the run before any cost is incurred. A track of unknown length goes to
  the provider, which checks it; probing durations at import is a follow-up.
- **A clip that follows its audio isn't priced per second of a setting.** Its length is the
  track's, not the card's `duration`, so the catalog refuses that pairing. Such a model is
  priced per call or by usage until a track's length can be counted (#608).
- **No carry-over without meaning.** When the model changes, a setting only carries over when
  both models tag it with the same meaning; a shared name alone (`seed`) is not enough.
