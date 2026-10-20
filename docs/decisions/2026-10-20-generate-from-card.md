## 2026-10-20 · Generate from the card, always through a quote (#83)

- **One path.** The card's arrow, Enter on a focused generation card and ⌘/Ctrl+Enter in its
  prompt all ask the app to generate that one card. The app dispatches `run.quote`, shows the
  totals, blocks and unknown part, and dispatches `run.start` only when the person presses
  **Generate for …**, passing the quote's token and totals. There is no shortcut that starts a
  paid run without the confirmation; agents and the CLI keep using the same two commands.
- **Unknown cost needs its own consent.** An incomplete quote enables Generate only after a
  separate checkbox, and sends `partial: true` with `unpriced_jobs` counted as the engine counts
  them (steps whose `unpriced` is a reason; a step with no job count counts as one). A quote whose
  steps are not all listed can't be accepted from the card.
- **Stale quotes are refused, not refreshed.** The confirmation remembers the session identity,
  canvas and revision. If any changes, it says so and offers *Quote again*; the engine would also
  refuse the old token.
- **Roles by meaning, not name.** The counter and the toolbar's aspect menu find their parameter
  by contract meaning (`generated_image_count`, `aspect_ratio`), falling back for aspect to a
  choice whose every option is `W:H`, so models with different parameter names share the controls.
  The counter and prompt are drawn in the composer and left out of the card's parameter list.
- **Composer below the image.** The prompt sits under the result rather than over it, so it never
  hides the image and keeps the card's existing layout and culling.
- **Not yet.** Text-to-image/image-to-image modes with ordered references, strength, prompt
  assistance, identity references and Download are follow-ups; the desktop app still installs no
  run host, so Generate there reports the engine's error until that lands (#409, #301).
