## 2026-10-17 · Programmatic ComfyUI JSON boundaries (#155)

- `ApiWorkflow::to_json`, `PromptRequest::to_json` and `discover` are fallible. Library
  conversion, input checking, mutation and discovery validate borrowed JSON before recursive
  copying or serialization: maximum depth 64 (root depth zero), 2,000,000 values plus object keys,
  and 32 MiB of encoded JSON including escaped strings, keys and outgoing envelope metadata.
  One iterative iterator stack uses at most 65 frames, regardless of array/object width. The
  item cap allows representative 10,000-node API and UI workflows with real sampler parameters;
  it interacts with the 32 MiB and depth caps, so not every 10,000-node combination necessarily
  fits. Large width tests use a private smaller item budget to keep fixtures economical.
- Conversion fan-out, repeated discovery metadata and check-report context share aggregate
  output budgets before retained copies. Accepted JSON is preserved; limits return value-free
  errors attributed to local input, not server responses. Input mismatch errors report types
  rather than arbitrary JSON or selected secrets. Discovery's 32 MiB admission budget includes
  the original workflow and copied exposure data together, so a large literal can fit `to_json`
  but fail `discover`. Only retained combo options are charged: upload combos expose Media
  without copying server input-directory listings, while their copied defaults still count.
  Rejected owned `set_input` values are disposed of iteratively, and mutation validates the
  prospective aggregate before changing the workflow.
- The public structs still expose caller-owned `serde_json::Value`. Calling raw derived
  `Clone`/`PartialEq`, formatting raw Values, or dropping caller-built excessively deep trees is
  the caller's responsibility; the guards protect the library entry points, not those external
  operations. Callers constructing hostile fixtures must dismantle them iteratively too.
  WebSocket progress/output retention (`files_in`) is a separate boundary and follow-up.

