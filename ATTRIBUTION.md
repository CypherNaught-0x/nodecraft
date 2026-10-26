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
| `assets/readme/` | README gallery frames, rendered offscreen from this app by `tools/eyecandy`; the scenes are built with engine commands and the procedural plates (dawn, aurora, bloom) are painted in code — no third-party image, no reference screenshot | NodeCraft contributors | this repository (`cargo run --manifest-path tools/eyecandy/Cargo.toml`) | CC0-1.0 |
| `tools/eyecandy/` | The offscreen renderer that produces those frames (original NodeCraft code, not adapted from another project) | NodeCraft contributors | this repository | MIT OR Apache-2.0 |
| `packaging/` | Release packaging for macOS, Windows, Linux and Flatpak, and the icon generator (adapted: NodeCraft ids and file types, no PSD/image associations, HEIF, font and riscv64 steps removed, network and media-folder Flatpak permissions, no DMG artwork, `.ico` packed with ImageMagick) | PhotoCraft contributors | [storytold/photocraft@2a952f1](https://github.com/storytold/photocraft/tree/2a952f164c0b4cf936b1c6497db10dd095341c00) `packaging/` | MIT OR Apache-2.0 |
| `assets/fonts/Lato-Regular.ttf`, `assets/fonts/Lato-OFL.txt` | Lato Regular, the layer editor's bundled text font (unmodified) | Łukasz Dziedzic (tyPoland) | [google/fonts@47d54ca](https://github.com/google/fonts/tree/47d54ca1ebb14fd864695f1d39dc734f39017c33/ofl/lato) `ofl/lato/` | OFL-1.1 |
| Linked into the app and wasm build through `nodecraft-media`'s `pdf` feature (`craft-pdf`, [storytold/craft-libs@8f5a283](https://github.com/storytold/craft-libs/tree/8f5a2831) `vendor/hayro-interpret/assets/Foxit*.pfb`) | The PDF standard-14 substitute fonts, used to draw PDFs that don't embed their fonts (unmodified) | PDFium Authors; original fonts © 2014 Foxit Software Inc. | [PDFium](https://pdfium.googlesource.com/pdfium/), via [LaurenzV/hayro](https://github.com/LaurenzV/hayro) | BSD-3-Clause |
| Linked in the same way (`hayro-cmap` 0.1.0, `assets/cmaps.brotli`) | Adobe's predefined CJK character maps, compressed, used to read the text of CJK PDFs (unmodified) | Adobe | [adobe-type-tools/cmap-resources](https://github.com/adobe-type-tools/cmap-resources), via [LaurenzV/cmap-resources](https://github.com/LaurenzV/cmap-resources/tree/hayro) | BSD-3-Clause |
| Linked in the same way (`vendor/hayro-interpret/assets/CGATS001Compat-v2-micro.icc`) | Compact CMYK ICC profile, used to draw CMYK PDF content (unmodified) | Clinton Ingram | [saucecontrol/Compact-ICC-Profiles](https://github.com/saucecontrol/Compact-ICC-Profiles) | CC0-1.0 |
| `assets/icons/tabler/search.svg` | Tabler outline search icon (unmodified) | Paweł Kuna and Tabler contributors | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline/search.svg) | MIT |
| `assets/icons/tabler/message-circle.svg` | Tabler outline message-circle icon (unmodified) | Paweł Kuna and Tabler contributors | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline/message-circle.svg) | MIT |
| `assets/icons/tabler/key.svg` | Tabler outline key icon (unmodified) | Paweł Kuna and Tabler contributors | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline/key.svg) | MIT |
| `assets/icons/tabler/wallet.svg` | Tabler outline wallet icon (unmodified) | Paweł Kuna and Tabler contributors | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline/wallet.svg) | MIT |
| `assets/icons/tabler/folder-open.svg` | Tabler outline folder-open icon (unmodified) | Paweł Kuna and Tabler contributors | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline/folder-open.svg) | MIT |
| `assets/icons/tabler/device-floppy.svg` | Tabler outline device-floppy icon (unmodified) | Paweł Kuna and Tabler contributors | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/icons/outline/device-floppy.svg) | MIT |
| `assets/icons/tabler/LICENSE` | Tabler icon license (unmodified) | Paweł Kuna | [tabler/tabler-icons@a4ce140](https://github.com/tabler/tabler-icons/blob/a4ce1404bc6d24d3c365afe7b258d6bf6f48d62d/LICENSE) | MIT |

## Tabler icon license

The notice below travels with the app in this attribution file, which every desktop package
includes. The unchanged license is also beside the source SVGs.

```text
MIT License

Copyright (c) 2020-2026 Paweł Kuna

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
