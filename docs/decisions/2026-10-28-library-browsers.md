## 2026-10-28 · The Library's browsers read the open document; they keep no index (#96)

- **An asset is a media value the document holds.** `library.assets` walks every card's
  parameters (through a few nested lists) and every result, and lists each distinct hash once
  with where it was first found and how often it is used. Asset bytes the store holds but no card
  references (such as one only an undone edit used) are not listed. Trained style
  descriptors are left to `library.styles`, and text values to `library.history`, so the Assets
  browser lists only images, videos, sounds, 3D models and PDFs.
- **No separate catalogue.** The listings are computed from the document on each call, so they
  can never disagree with it, need no persistence or migration, and follow undo for free. The
  cost is a walk of the open document, which the document's own size limits bound; every listing
  returns at most 200 entries a page, with the total, and the app asks for 20.
- **Read-only commands; acting reuses existing ones.** The browsers add no edit path: inserting
  an asset is `node.add` of an instantiated source card (agents can use `node.create` and
  `node.param_set`), a result is restored with `history.select` and placed with
  `history.extract` (the listing names the `port` and `item` it needs), and a style is wired
  with `style.use`. Every change is therefore one existing, already-hunted undo step.
- **Styles are listed with the provider that keeps them.** A trained style or Soul ID is a
  handle to weights or an identity held by one provider account (`2026-10-21-trained-styles.md`,
  `2026-10-22-soul-id-training.md`); `kept_by` names that provider and `identity` marks the
  `soul-2` family, so the browser can say the handle works only with that account.
- **One project.** There is no store of other projects to browse; cross-project browsing waits
  for one (and for the global asset library the issue describes), together with URL import.
