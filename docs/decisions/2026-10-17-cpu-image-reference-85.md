# 2026-10-17 · CPU image reference (#85)

- **RGBA8 SDR with explicit linear-light math.** Immutable capacity-checked rasters share a
  private Arc<Vec<u8>>. Exact identity/permutation paths preserve bytes; computed alpha-zero
  pixels are canonical. No implicit HDR clipping or ICC fidelity is claimed.
- **Admission precedes decode/conversion and kernels.** Named input roles, actual capacities,
  inferred output shape, compound work and all worker scratch share bounded preflight.
  Encoded adapters reserve decoder/orientation/RGBA overlap before pixels are decoded.
- **Every named blur is an original finite CPU reference.** Fixed sample order, bounded
  row-ring convolution, explicit parameter minima and deterministic bokeh make native/wasm
  semantics reproducible. Native rayon is an explicit fallible private pool; wasm is serial.
- **The inventory belongs to #162.** Typed action parameters and `image-actions.md` specify the
  normative controls without a second descriptor engine. #88 owns live graph/command adapters;
  #89 owns GPU parity. Split into grid remains #86.
