# Releasing

Releases are GitHub releases built by the shared Craft workflow
(`storytold/craft-common-actions/.github/workflows/release.yml@main`), called from
[`.github/workflows/release.yml`](../.github/workflows/release.yml). It runs this repository's
`packaging/` scripts on every platform and drafts a release `NodeCraft vX.Y.Z` with all artifacts
and `SHA256SUMS.txt`.

## Cutting a release

1. `cargo xtask version set X.Y.Z` (or `X.Y.Z-rc.N` for a pre-release), commit, merge to `main`.
2. Merge `main` into the `release` branch and push. The workflow builds a **draft** release.
3. Check the draft's artifacts, then publish it (publishing creates the tag `vX.Y.Z`).

A test build without signing: Actions › Release › Run workflow, on any branch other than
`release`, optionally with a version override.

## Signing

On the `release` branch the shared workflow **fails** unless the signing secrets are set in the
repository's `release` environment:

- macOS: `APPLE_CERTIFICATE` (base64 .p12), `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD`,
  `APPLE_ID`, `APPLE_PASSWORD` (app-specific), `APPLE_TEAM_ID`.
- Windows: `WINDOWS_CERTIFICATE` + `WINDOWS_CERTIFICATE_PASSWORD`, or the Azure Trusted Signing set
  (`AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_SIGNING_ENDPOINT`,
  `AZURE_SIGNING_ACCOUNT`, `AZURE_CERT_PROFILE`).

Manual runs on other branches build ad-hoc/unsigned artifacts with a warning.

## Artifacts

| Platform | Built by | Files |
|---|---|---|
| macOS universal | `packaging/macos/package.sh --arch universal` | `nodecraft-<v>-macos-universal.dmg`, `nodecraft-cli-<v>-macos-universal.zip` |
| Windows x64, x86, arm64 | `packaging/windows/package.ps1 -Arch <arch>` | `.msi`, `-portable.zip` (both binaries) |
| Linux x86_64, aarch64 | `packaging/linux/package.sh` | `.AppImage` (+ `.zsync`), `.deb`, `.rpm`, `.tar.gz` |
| Linux Flatpak | `packaging/linux/flatpak-bundle.sh` (repackages the tarball) | `.flatpak` |
| FreeBSD x86_64 | the shared workflow's generic build | `.tar.gz` |

Turned off in `release.yml`: the web bundle (`has-web`, until `apps/nodecraft-web` exists) and
Linux riscv64 (`has-riscv64`; not a NodeCraft target, and `packaging/linux/package.sh` has no
cross-build support).

## Local packaging

```sh
packaging/macos/package.sh --arch aarch64     # ad-hoc signed DMG + CLI zip in dist/release/
packaging/linux/package.sh --formats tar      # on Linux; deb/rpm need nfpm
pwsh packaging/windows/package.ps1 -Arch x64  # on Windows; needs WiX v5
packaging/icons.sh                            # after editing assets/app-icon/nodecraft.svg
```

The scripts read the version from `Cargo.toml` and stamp `NODECRAFT_BUILD_SHA` and
`NODECRAFT_BUILD_DATE` into the binaries (`packaging/env.sh`).
