## 2026-10-16 · Graph transactions (#51)

- **Stored document maps are private.** Detached node data is editable; a stored document changes
  only through serializable changesets. Deserializing a project runs structural validation.
- **Changesets are ordered and atomic.** Existence, payload and count checks apply per edit;
  relationships are checked once at commit. Temporary cycles, occupied input orders or missing
  field/tag references can be repaired within the batch. New edges require existing nodes,
  and carried snapshots must pass payload validation. Staging one clone keeps failed edits from
  partially mutating the live document. Inverses use the same bounded path as forward edits.
- **History edits are granular.** Indexed generation insertion/removal and active selection
  preserve history order without serializing the whole node in every undo record.
- **Aggregate JSON budgets cover both directions.** Nodes are capped at 8 MiB, changesets and
  their generated inverses at 64 MiB, and projects at 256 MiB. Reject an oversized undo before
  publishing its forward edit. These limits and graph/journal variants belong to the file
  version contract; check that version before deserializing and overwriting a project.
- **Connections form a DAG.** Ports must have matching modalities, a list output cannot feed a
  single input, and each input order is unique. Cycles are rejected with an iterative walk.
- **Node deletion has a compact inverse.** One restore operation reinstates the node and its
  incident edges, avoiding an undo operation-count overflow for highly connected nodes.

