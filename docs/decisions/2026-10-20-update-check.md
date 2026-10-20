## 2026-10-20 · Update checks notify and never install (#123)

- **Notify only.** `app.check_updates` reads the GitHub release list and the desktop app shows a
  banner with the release notes link; it downloads, verifies and installs nothing. An updater
  that replaces the binary needs signed update feeds and per-OS installers (MSI, a macOS app
  bundle, AppImage or Flathub), which belong with the release pipeline, not the app (#540).
  Flatpak, Snap and Linux system-package installs (an executable under `/usr`, not
  `/usr/local`) are told to update through their package manager; Homebrew and winget installs
  aren't detected yet and see the plain banner.
- **GitHub releases are the feed.** No NodeCraft server exists, so the check reads
  `api.github.com/repos/CypherNaught-0x/NodeCraft/releases` with the generic User-Agent and no
  identifier. The reply is hostile: size-capped, drafts and tags that aren't a clean semver
  version (build metadata included) skipped, and the reply's own page links never used: the
  link the banner opens is built from the validated tag, so it always leads to NodeCraft's
  releases page.
- **Two channels.** `stable` (the default) sees finished releases; `prerelease` also sees
  release candidates. Versions compare as semver, and a release sorts after its candidates.
- **Quiet at startup.** One check runs at startup unless `updates.check` is off; it says nothing
  when up to date or offline. Dismissing a release stores its version, so startup doesn't offer
  it again; asking explicitly always shows what is available.
- **Native only.** The browser build is always served current, so the command reports itself
  unsupported there and in the CLI.
