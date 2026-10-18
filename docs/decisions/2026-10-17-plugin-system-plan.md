## 2026-10-17 · Plugin system plan (#154)

- **Plan before runtime.** [The plugin RFC](plugins-rfc.md) accepts data-only packages as the first
  distributable scope. Third-party executable actions require a safe-API sandbox feasibility gate;
  no runtime engine, ABI, signature algorithm or marketplace is selected in this documentation PR.
- **One host authority.** Plugin code/data cannot mutate a Session, bypass descriptor validation,
  read raw credentials or submit paid calls outside host quote/budget/idempotency brokers. Schema
  semantics stay with #162; catalog data stays with #77 rather than a plugin-specific variant.
- **Portable preservation precedes execution.** Exact digest locks and bounded missing-plugin
  placeholders retain unknown work. Installation never grants rights or executes project content.
  Browser-unsupported capabilities are explicit; compilation alone does not prove runtime parity.
- **No Rust dynamic ABI.** The MVP avoids native dynamic loading and local unsafe code. Runtime
  selection/resource targets remain measured, reviewed phase deliverables, not shipped guarantees.
