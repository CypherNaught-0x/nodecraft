## 2026-10-27 · Headless named inputs, isolated CSV rows and ComfyUI import (#97)

Fill project/Technique inputs through existing engine commands and typed parameter/form schemas.
A project target uses source titles/ids and explicit parameter names; a Technique target selects
a non-retired published version in the project or local library and places it on a scratch
canvas. Neither path changes the source file or library. Ambiguous names and duplicate aliases
fail before provider setup. No host-specific graph mutation bypasses the commands.

Bound CSV parsing, typed text and snapshotted media before provider setup; preflight every row's
quote/consent and the aggregate budget before generation. Share a runner and usage budget while
using independent Sessions, bindings and routing preparation for every row. Create unique row
folders before spending; default to fail-fast and require an explicit flag to continue.

Use the existing free API-key checks for provider testing and the existing ComfyUI refresh/import
commands for workflow conversion. Import publishes a new archive atomically without replacing
an existing output; source archives and credentials never become writable destinations.

Native runs/imports use the existing quota-accounted NativeStorage protocol with a unique owned
recovery path beside their output/source. Discard it on normal completion, including errors;
never recursively delete asset workspaces while leases may survive. Retain/report failed cleanup.
Pure read-only commands keep their old bounded path. This preserves streaming results without
staging large projects in system temp or inventing a second journal protocol. Forced termination
can leave the named private recovery journal; document that boundary and never silently remove
an unknown journal. See the [CLI contract and limits](../cli.md).
