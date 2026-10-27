# Official template field mappings

`template-mappings.v1.json` is data consumed by `nodecraft-comfy`. Version 1 records the pinned
upstream commit, original template blob ids, pinned usage, structural graph contracts, and field
and result selectors. It contains no prompt defaults, media assets or model selections. See the
[survey](../../docs/comfy-template-survey.md) and
[decision](../../docs/decisions/2026-10-27-official-comfy-template-mappings.md).

The structural data is derived from Comfy Org's MIT-licensed workflow templates; the unchanged
notice is in `LICENSE`. The mapping choices are NodeCraft's own. Candidate video profiles remain
under validation; these files are work in progress until #948's acceptance criteria are met.

Live ComfyUI validation is an explicit user-required merge blocker. Keep the PR draft until
that validation is recorded, even if every offline gate passes.

Fields select an API `node`/`input`, a promoted subgraph `instance`/`input`, or a root `node`/`widget`
ordinal. Ordinals require a node with no UI input sockets and the exact pinned widget count;
the discovered control kind must also match. An omitted `label` retains the server label and
units. This is useful for selector nodes whose saved widgets do not name their parameters.
Changed counts, types or linked fields prevent curation; they do not shift the selector.
