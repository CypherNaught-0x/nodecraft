## 2026-10-17 · Instance schemas and provider-ready controls (#53)

- **Checked instance hooks complement static palette descriptors.** Resolve validates bounded
  node config before dynamic code and validates its descriptor before control/port cloning.
  Node-aware modes support workflows and Techniques; pure source planning uses this same path.
- **Configuration is kind-owned, namespaced and versioned.** A separate defaulted `Node.config`
  map retains unknown typed data under graph budgets. SetNode is its mutation path. Explicit
  migration proposals carry warnings and never silently mutate a saved node during planning.
- **Numeric schemas preserve full-width integers.** Optional bounds, unsigned controls, exact
  positive integer steps and seed maxima describe provider constraints without f64 coercion.
- **Hidden values persist but are not execution inputs.** ResolvedParams exposes visible-only
  adapters. All schema text, including large model choice lists and descriptions, shares a cap.

