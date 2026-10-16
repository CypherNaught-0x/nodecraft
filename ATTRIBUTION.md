# Attribution

Every third-party asset and every piece of code adapted from another project is listed here, in
the same change that adds it (CONTRIBUTING.md, "Clean-room").

| Path | Title | Author | Source | License |
|---|---|---|---|---|
| `scripts/claim/claim.js`, `scripts/claim/claim.test.js`, `.github/workflows/claim.yml` | `/claim` issue bot (adapted: NodeCraft paths, test fixtures; the workflow's comment filter uses `contains` so it matches the script's whitespace-tolerant parser) | SolidCraft contributors | [bherbruck/solidcraft@77b33ef](https://github.com/bherbruck/solidcraft/tree/77b33efd8f0e8498f4d0d00697419d0992150759/scripts/claim) | MIT OR Apache-2.0 |
| `xtask/src/layers.rs`, `xtask/src/version.rs`, `xtask/src/main.rs` | Workspace tooling: layering check, version management, CI runner (adapted: NodeCraft's layer table, UI-crate allow-list instead of a UI layer, one batched wasm check) | PhotoCraft contributors | [storytold/photocraft@2a952f1](https://github.com/storytold/photocraft/tree/2a952f164c0b4cf936b1c6497db10dd095341c00) `xtask/` | MIT OR Apache-2.0 |
| `crates/engine/src/build_info.rs` | Build provenance for `--version` and About (adapted: NodeCraft names, char-safe SHA shortening) | PhotoCraft contributors | [storytold/photocraft@2a952f1](https://github.com/storytold/photocraft/tree/2a952f164c0b4cf936b1c6497db10dd095341c00) `crates/engine/src/build_info.rs` | MIT OR Apache-2.0 |
| `apps/nodecraft/build.rs` | Windows icon and VERSIONINFO resources (adapted: NodeCraft names) | PhotoCraft contributors | [storytold/photocraft@2a952f1](https://github.com/storytold/photocraft/tree/2a952f164c0b4cf936b1c6497db10dd095341c00) `apps/photocraft/build.rs` | MIT OR Apache-2.0 |
| `assets/app-icon/` | NodeCraft app icon (original artwork; every file generated from `nodecraft.svg`) | NodeCraft contributors | this repository | CC0-1.0 |
| `packaging/` | Release packaging for macOS, Windows, Linux and Flatpak, and the icon generator (adapted: NodeCraft ids and file types, no PSD/image associations, HEIF, font and riscv64 steps removed, network and media-folder Flatpak permissions, no DMG artwork, `.ico` packed with ImageMagick) | PhotoCraft contributors | [storytold/photocraft@2a952f1](https://github.com/storytold/photocraft/tree/2a952f164c0b4cf936b1c6497db10dd095341c00) `packaging/` | MIT OR Apache-2.0 |
