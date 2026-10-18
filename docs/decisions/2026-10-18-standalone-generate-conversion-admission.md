
## 2026-10-18 · Standalone Generate conversion admission (#279)

- Public `provider_job` accepts standalone jobs, so document validation cannot stand in for
  admission. Before copying anything proportional to caller data, it checks all parameter and
  input values: depth 32, 4,096 aggregate visits (including empty lists), 4,096 flattened inputs,
  and 1 MiB aggregate text per original value. Media inputs remain unsupported.
- A shared 64 MiB request representation/work budget charges each string as six times its
  UTF-8 length plus 34 bytes, each value 128 bytes, and each input root and emitted leaf 64
  bytes plus its role string. Names, provider, model and idempotency key count too. Repeated
  flattened roles count each time. These are conservative upper bounds, not exact wire or RSS
  measurements; combining individually valid roots can now be refused.
- Result admission checks all routes and outputs before cloning text, hashing media or cloning
  asset Arcs. It allows at most 4,096 routes/results, 1 MiB routed text in total, 256 MiB per
  binary item and aggregate raw payload, and 8 MiB converted representation/work. Every Arc
  occurrence counts, even if shared. Routes cost 64 plus their name, records 128, strings use
  the same formula, and media references cost 512 plus sniffed MIME and port. Claimed MIME
  counts as work. Late unsupported, invalid UTF-8 or unroutable outputs refuse the whole
  conversion before publishing a valid prefix. Accepted media keeps the original shared bytes.
- Iterative admission keeps only depth-bounded traversal state. Conversion recurses only after
  admission. Refusal leaves the original job/paid result with its caller. These checks do not
  certify a complete Generation's port types/cardinality or persisted asset availability.

