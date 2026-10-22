## 2026-10-22 · A staged update is checked again before it is used (#567)

Builds on [the signed update feed](2026-10-21-update-install.md).

- **Checked at the swap, not only at download.** A staged update can wait days for the restart
  in a folder the person (and anything running as them) can write to. Just before the swap the
  macOS bundle's code signature is verified again and its code directory hash (CDHash) compared
  with the one recorded when it was staged, since a changed bundle can be validly re-signed ad
  hoc; the AppImage and MSI are hashed again against the signed feed's BLAKE3, reading no more
  than its signed size. A changed file is removed and nothing is installed. The
  Windows helper gets the hash as `--finish-update <msi> <blake3> <exe>` and checks the MSI once
  more while holding it open so that other programs can only read it, so it can't be replaced
  between the check and `msiexec`.
- **The channel is signed.** The feed carries `prerelease` (a version with a pre tag, as the
  release job decides), and a payload whose feed says prerelease is refused when it was offered
  as a stable release. Today the signed version already says the same (a pre tag marks the
  release a prerelease), so this is defence in depth that keeps holding if the two rules ever
  diverge. The field is required: no released app reads feeds yet, and every feed is written by
  a release job that has it. Because the feed denies unknown fields, any later field needs a
  `format` bump, which older apps report as a feed they can't read.
- **Cleanup waits for other copies.** Every running copy holds, from the start of its update
  checker until it exits, a shared lock on `instances.lock` in the updates folder; a copy cleans
  up leftovers at startup only if it could take that lock alone first, since another copy's
  staged update looks like a leftover. Without a lock (an unwritable folder) nothing is removed.
  That lock is per user, but an app folder can be shared, so on macOS each copy also holds a
  shared lock on its own executable, and a renamed old bundle whose executable is locked (another
  user still runs it) is kept.
- **`--control` survives the restart.** A copy started with `--control` starts the updated one
  (and the Windows helper passes on) `--control` too, so automation reconnects.
