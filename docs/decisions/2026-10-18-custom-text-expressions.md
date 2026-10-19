## 2026-10-18 · Custom text actions use a bounded expression interpreter (#165)

- **An interpreter, not Wasm.** A Wasm runtime that meters fuel and builds for both native and
  `wasm32-unknown-unknown` without `unsafe` in our tree would mean a large dependency and a
  second toolchain for authors. Custom text transforms are short string and list manipulations,
  so a small, original expression language in `nodecraft_actions::expr` covers them. It is pure
  Rust, has no new dependencies, and runs identically native and in the browser. A Wasm boundary
  can still come later through the plugin work (#154/#173) for heavier logic.
- **Deterministic by construction.** Expressions see only their inputs and parameters: no
  filesystem, network, process, clock or randomness. Numbers are 64-bit integers with checked
  arithmetic (no floats), text is indexed by Unicode scalar values, and `sort`/`unique` compare
  by value. The same definition and inputs give the same result everywhere.
- **Budgets, not timeouts.** Parsing bounds source size, nodes, nesting (32) and tree depth
  (96), and argument evaluation is split from the large function bodies, so the deepest allowed
  expression fits a 1 MiB stack even in debug builds. Running bounds fuel (one budget across all
  outputs), every produced value's size, list items, input size and nesting, and the bytes held
  at once (names, operands, arguments, partial results). Cancellation is polled from fuel use.
- **Portable definitions.** `TextActionDefinition` (schema 1) declares a stable dotted id, a
  version, typed `text`/`text_list` inputs, `text`/`int`/`bool` parameters with defaults, and
  one typed expression per output; unknown fields are refused. Compiling checks every name an
  expression reads.
- **Private by default.** Errors carry a fixed reason, a byte offset into the expression and
  the declared name involved, never input or parameter values.
- **Phase 1 scope.** This is the runtime and the definition format. The node kind, parameter
  forms (#162), the definitions library and persistence (#96), live preview (#88), creation
  through a quoted provider call and Technique use (#93/#94) build on it later. Built-in text
  actions are not node kinds yet either.
