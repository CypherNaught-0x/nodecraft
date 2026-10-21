## 2026-10-21 · Updates install from a signed feed, per platform (#540)

- **Our own small updater.** Velopack needs its own packaging and `Update.exe`, and Sparkle an
  Objective-C bridge (and so `unsafe`); both replace packaging we already ship. The app instead
  installs the artifacts every release already has: the macOS app bundle (zipped, new), the
  AppImage and the MSI. The deb, rpm, Flatpak, portable zip and anything under `/usr` keep the
  notify-only banner (#123).
- **A signed feed, checked twice.** Each release carries `nodecraft-update.json` (format, version,
  and per platform the payload's file, size and BLAKE3 hash) and `nodecraft-update.json.sig`, an
  Ed25519 signature of `NodeCraft update feed v1\n` + the feed. `cargo xtask update-feed` writes
  both in the release job from the secret `NODECRAFT_UPDATE_SIGNING_KEY`; the public keys are
  `packaging/update-key.pub`, compiled in (several, so the key can be rotated). The app refuses a
  feed whose signature, format or version doesn't match the release it was offered (no replaying
  an old feed), and a payload whose size or hash differs; downloads are capped at the size the
  signed feed states. A build without a key never installs. TLS and GitHub are not trusted for
  integrity; the key is.
- **Two steps, both commands.** `app.install_update` downloads, verifies and stages; nothing of
  the running copy changes. `app.restart_to_update` swaps it in and restarts, refused while a
  generation runs or waits. Both are host services, so the CLI and web report them unsupported;
  they aren't offered to the in-canvas agent.
- **Swapping per platform.** macOS: unzip (one bundle, no symlinks, bounded) beside the running
  bundle, `codesign --verify --deep --strict`, then two renames on the same volume, with rollback;
  a copy running from a disk image or App Translocation is told to move to Applications first.
  AppImage: write beside, rename over (the running process keeps its file). Windows: a copy of the
  executable runs `--finish-update`, waits for the app to exit, runs the MSI `/passive` (its
  `MajorUpgrade` replaces the old version) and starts NodeCraft again; a cancelled or failed
  installer is reported at the next start.
- **The project survives.** The window closes normally (saving settings, keeping the recovery
  journal for unsaved work), then the new copy starts with `--wait-for <pid>` and the project
  path, so it opens the project only after the old process has let go of it. `last-update.json`
  in the settings folder's `updates/` says at the next start whether the update landed; leftovers
  (the old bundle, half-staged copies, the helper) are removed then, matched by the names the
  updater makes and nothing else.
- **Debug-only overrides.** `NODECRAFT_UPDATE_FEED` (a base URL serving `releases.json` and
  `<tag>/<file>`) and `NODECRAFT_UPDATE_KEYS` (a key file) work in debug builds only, for testing
  against a local feed; release builds read and trust only NodeCraft's releases and keys.
