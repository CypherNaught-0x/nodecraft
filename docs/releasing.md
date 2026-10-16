# Releasing

Releases are GitHub releases built by [`.github/workflows/release.yml`](../.github/workflows/release.yml).
It follows the layout of the shared Craft release workflow
(`storytold/craft-common-actions`) but is our own, so that **missing signing secrets don't block a
release**: it runs this repository's `packaging/` scripts on every platform and drafts a release
`NodeCraft vX.Y.Z` with all artifacts, `SHA256SUMS.txt`, and `packaging/release-notes.md` (how to
open unsigned builds) above the generated changelog.

## Cutting a release

1. `cargo xtask version set X.Y.Z` (or `X.Y.Z-rc.N` for a pre-release), commit, merge to `main`.
2. Merge `main` into the `release` branch and push. The workflow builds a **draft** release.
3. Check the draft's artifacts, then publish it (publishing creates the tag `vX.Y.Z`).

A test build without signing: Actions › Release › Run workflow, on any branch other than
`release`, optionally with a version override.

## Signing

Signing is optional. Each secret below, set in the repository's `release` environment, switches
on its step; without them the scripts print a warning and the release still builds:

- macOS (Developer ID signing + notarization): `APPLE_CERTIFICATE` (base64 .p12),
  `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD` (app-specific),
  `APPLE_TEAM_ID`. Without them the app is ad-hoc signed and not notarized.
- Windows (Authenticode): `WINDOWS_CERTIFICATE` + `WINDOWS_CERTIFICATE_PASSWORD`, or the Azure
  Trusted Signing set (`AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
  `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE`). Without them the MSI
  and executables are unsigned.

Unsigned builds show a first-launch warning; [`install.md`](install.md) explains how users get past
it, and every release's notes link there. When signing lands (#100), drop the warning from
`packaging/release-notes.md` and `install.md`.

## Artifacts

| Platform | Built by | Files |
|---|---|---|
| macOS universal | `packaging/macos/package.sh --arch universal` | `nodecraft-<v>-macos-universal.dmg`, `nodecraft-cli-<v>-macos-universal.zip` |
| Windows x64, arm64 | `packaging/windows/package.ps1 -Arch <arch>` | `.msi`, `-portable.zip` (both binaries) |
| Linux x86_64, aarch64 | `packaging/linux/package.sh` | `.AppImage` (+ `.zsync`), `.deb`, `.rpm`, `.tar.gz` |
| Linux Flatpak | `packaging/linux/flatpak-bundle.sh` (repackages the tarball) | `.flatpak` |

Not built yet: the web bundle (add a `web` job with the web app, #99). Not targets: Windows x86,
Linux riscv64 and FreeBSD (the shared Craft workflow builds those for other apps).

## Local packaging

```sh
packaging/macos/package.sh --arch aarch64     # ad-hoc signed DMG + CLI zip in dist/release/
packaging/linux/package.sh --formats tar      # on Linux; deb/rpm need nfpm
pwsh packaging/windows/package.ps1 -Arch x64  # on Windows; needs WiX v5
packaging/icons.sh                            # after editing assets/app-icon/nodecraft.svg
```

The scripts read the version from `Cargo.toml` and stamp `NODECRAFT_BUILD_SHA` and
`NODECRAFT_BUILD_DATE` into the binaries (`packaging/env.sh`).
