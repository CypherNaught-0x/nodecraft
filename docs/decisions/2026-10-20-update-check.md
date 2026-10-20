## 2026-10-20 · Update checks notify and never install (#123)

- **Notify only.** `app.check_updates` reads the GitHub release list and the desktop app shows a
  banner with the release notes link; it downloads, verifies and installs nothing. An updater
  that replaces the binary needs signed update feeds and per-OS installers (MSI, a macOS app
  bundle, AppImage or Flathub), which belong with the release pipeline, not the app. Flatpak,
  Snap and system-package installs are told to update through their package manager.
- **GitHub releases are the feed.** No NodeCraft server exists, so the check reads
  `api.github.com/repos/CypherNaught-0x/NodeCraft/releases` with the generic User-Agent and no
  identifier. The reply is hostile: size-capped, drafts and non-version tags skipped, and a
  release whose page isn't under NodeCraft's releases page is ignored, so the one link the
  banner opens always leads there.
- **Two channels.** `stable` (the default) sees finished releases; `prerelease` also sees
  release candidates. Versions compare as semver, and a release sorts after its candidates.
- **Quiet at startup.** One check runs at startup unless `updates.check` is off; it says nothing
  when up to date or offline. Dismissing a release stores its version, so startup doesn't offer
  it again; asking explicitly always shows what is available.
- **Native only.** The browser build is always served current, so the command reports itself
  unsupported there and in the CLI.
