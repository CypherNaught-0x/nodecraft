

## 2026-10-18 · Bounded hybrid GPU image actions (#89)

- The L3 GPU library consumes the CPU action inventory and shared borrow-bound admission;
  command/live graph wiring remains #88. It does not introduce a second parameter schema.
- f64 hard decisions and exact alpha/nearest addressing are prepared on the CPU, with charged
  optional planes. Continuous kernels run on wgpu, with explicit fixture tolerances and no
  hidden CPU fallback. Exact paths preserve bytes; computed zero-alpha RGB is canonical.
- Separable effects use adaptively admitted global-halo stripes. Blurred padding retains its
  immutable packed cover until all stripes finish, preserving both reference quantizations.
- Only matched device/queue pairs acquired together are accepted. Native progress is driven by
  a separately acquired device-only poll handle; dropped futures retain bounded pending work.
  GPU failures are scoped Results. Existing eframe-pair import is not promised.
