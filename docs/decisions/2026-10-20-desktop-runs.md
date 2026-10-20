## 2026-10-20 · The desktop app runs Generate on the engine's run host (#409)

- **A tokio runtime in `ui-egui`, native only.** `app::runs::Runs` owns a two-worker tokio
  runtime and implements the engine's `Runtime` on it. The adapters use reqwest, which needs
  tokio anyway. The wasm build can quote but not run yet; its runtime is separate work. When
  the window closes, open runs are cancelled and the runtime shuts down without waiting for slow
  providers.
- **Adapters are built on their own thread.** Building an adapter reads its key from the OS
  keychain, which can show an unlock prompt. So `provider_setup::make` runs on a
  `nodecraft-providers` thread, the card says "Setting up the provider…", and the quote opens on
  the frame after the adapter is ready. A panicking keychain backend fails that adapter, not the
  thread. Both channels are bounded, and more asks than fit fail as busy.
- **Each Generate routes every ready card on the canvas.** The session's provider routing is
  rebuilt with every card whose adapter is ready, so a run that pulls in other cards reaches
  their providers too, not only the clicked card's.
- **A provider that can't run is asked again next time.** A missing key, or a server that
  isn't set up, shows its message and is forgotten. The next Generate reads the keychain afresh,
  so adding the key in Settings is enough. Saving or deleting any key forgets every built
  adapter for the same reason.
- **Cards show what their run is doing.** "Running…" while the host has the card's job, and
  "Run failed" (the reason on hover) until the card runs again. A refused start or a failure
  also shows in the notice.
- **Not yet:**
  - budgets come from `Budgets::default()`, not from settings;
  - the usage log lives in memory for the session only (#508), so the spend that budgets and
    quotes count starts at zero on each launch.
