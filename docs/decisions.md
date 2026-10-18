# Decisions

Decisions others will build on, newest first. The product-level ones are in
[`plan.md`](plan.md) §6; this log records the ones taken while building. Add an entry when you make
a choice a later contributor might otherwise undo.


Decision records now live in [`decisions/`](decisions/). Dated file names sort by date; read
newest first when tracing recent choices. The folder listing is the canonical index.

## Add a decision

Create one new file named `decisions/YYYY-MM-DD-short-descriptive-slug.md`, using the decision's
date and a unique topic slug. Start it with `## YYYY-MM-DD · Title (#issue)`, then explain the
choice and its consequences. Use links relative to that new file (for example, `../plan.md`).

Do not append to this index or maintain a shared list of entries. Independent decisions should
only add their own files, so parallel changes do not conflict on a common insertion point.
Read existing records before changing a choice others rely on.

Historical records retain their original text, including superseded limits and compatibility
claims. Only `YYYY-MM-DD-*.md` files are decision records. The undated
[`plugins-rfc.md`](decisions/plugins-rfc.md) is a navigation-only compatibility redirect for an
unchanged link in an older record; the RFC itself remains in [`plugins-rfc.md`](plugins-rfc.md).

The former log's introduction above is retained. Bare links to this page still work; old
section-anchor bookmarks should be updated to the corresponding dated file in the folder.
