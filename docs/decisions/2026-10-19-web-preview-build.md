## 2026-10-19 · Web preview build (#99)

- **`apps/nodecraft-web` is a thin app crate.** Its binary starts `NodeCraftApp::new` on eframe's
  `WebRunner` in the page's `<canvas>`; a failed start (no WebGPU or WebGL, say) replaces the
  loading line with the reason. All dependencies are `cfg(target_arch = "wasm32")`, and the
  native binary only prints how to build it, so native workspace builds stay cheap.
  `cargo xtask wasm` checks it with the layered crates.
- **trunk + wasm-bindgen, offline.** `packaging/web/package.sh` runs `trunk build --offline
  --locked --release` with the `wasm-release` profile (`data-cargo-profile`), so trunk never
  downloads tools: `wasm-bindgen-cli` must match the version in `Cargo.lock`, and the release job
  installs it, a pinned trunk, and a checksum-verified binaryen `wasm-opt`.
- **wasm-opt runs after trunk, without subresource integrity.** trunk's own wasm-opt step would
  download binaryen; running it in the script instead means the `.wasm` changes after trunk
  hashed it, so `index.html` carries no `integrity` attribute (`data-integrity="none"`). Without
  wasm-opt the script warns and ships the module as built.
- **Per-file cap instead of splitting.** The plan said "split into parts under host size caps".
  Splitting the `.wasm` would need a hand-written JavaScript loader, which the Rust-only rule
  forbids, so the script instead fails when any file is over 25 MiB (Cloudflare Pages' per-file
  cap; GitHub Pages and Netlify allow more). At 0.2.0-rc.1 the
  `.wasm` is 8.6 MB after wasm-opt (11.7 MB before), and the zip 3.6 MB.
- **Relative public URL.** The site is built with `--public-url ./`, so the unzipped folder works
  from any path on any static host.
- **The release workflow is our own.** There is no `has-web` flag to set (that belongs to the
  shared Craft workflow, which we don't use yet, see `2026-10-16-m0-skeleton.md`); `release.yml`
  gains a `web` job whose zip the draft release includes.
- **Preview scope.** The browser window opens the starter canvas with in-memory settings. Project
  files, keys and runs need browser hosts (File System Access, origin storage, fetch-based
  providers; `plan.md` §7) and are later work; their commands already report *unsupported*.
