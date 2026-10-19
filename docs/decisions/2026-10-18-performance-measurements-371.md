## 2026-10-18 · Graph stage and rendered-curve performance measurements (#371)

- **Name the actual boundary.** Report schema `nodecraft.perf/2` adds successful/refused
  `Changeset::stage_certified` timings: clone/apply/validation/inverse work including the inverse
  certification probe. Subsequent actual-redo replay/history admission, final pointer publication
  and durable append are outside this span. Availability previews do not record. Completed staging survives later
  admission refusal; overall dispatch remains success-only. These boundaries overlap.
- **Visibility follows the drawing geometry.** The paint snapshot records submitted persistent
  cubics against the actual painter clip using the renderer's adaptive flattening tolerance.
  A streaming intersection test includes the widest painted stroke/halo. For pinned epaint
  0.36.2 OPEN paths, cut joins have normal length at most two; non-AA radius is width/2,
  thin-AA radius is feathering, thick-AA radius is (width+feathering)/2 with endpoint caps.
  Width + 2*feathering + tolerance is a conservative axis margin. This can overcount at
  corners/joins/AA; it is not pixel visibility or occlusion. Invalid observations are null
  with a reason. The first point is seeded before adaptive callbacks. Disabled capture does
  no diagnostic flattening; the renderer and its ordinary cull/curve submission are unchanged.
- **Unsupported is explicit.** Presentation timestamps remain unavailable on every current
  host. The public recorder cannot inject samples into that unsupported row. Frame-interval
  over-budget counts remain an idle-filtered gap heuristic, not presentation evidence.
- **Measure collection, not only pushes.** Whole Session dispatch and UI show/tessellation
  paired benchmarks include callbacks and workload geometry. The older recorder-push loop is
  only a microbenchmark. Fixed public fixtures, ordered paired observations and no CI timing
  threshold preserve reproducibility without claiming universal overhead or GPU timing.
- **Retain the privacy and ownership boundary.** Recording is opt-in and bounded to 4,096
  samples per finite metric. Frame observations and geometry state retain only counts/ticks;
  no document/changeset/error clone or per-edge geometry buffer is held by the recorder.
