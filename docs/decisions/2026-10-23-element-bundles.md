## 2026-10-23 · Elements are shared as bundle files, remember where they were placed from, and a damaged entry is set aside (#765)

- **Sharing is a file, not a sync.** `library.export_element` writes one entry as an Element
  bundle (`.ncelement`): a ZIP of stored files holding `element.json` (format
  `nodecraft.element`, version 1, the entry) and `media/<blake3>` for each image. Nothing else
  goes in: no keys, accounts, projects or history. `library.import_element` adds a bundle to the
  library. Every image is checked against the hash it is named by, the entry is checked as a
  saved one is, and a bundle with anything else in it (other files, links, encrypted entries,
  extra or missing images, a newer version) is refused. Reads are capped by the size of an
  Element's largest images. Syncing shared Elements to a team through live sharing is left for
  later.
- **A bundle arrives private.** Export clears `shared` and import keeps it clear: whether to
  share is the receiver's call. `library.share_element` sets and clears the flag.
- **A name clash is the caller's to settle.** Import refuses an entry whose name the library
  has, unless it is given `as` (another name) or `replace`. The panel offers Replace or Cancel;
  renaming the library's entry first keeps both.
- **Export, import and share are a person's.** Like saving and removing, they change the library
  every project shares, and export and import touch files on the machine, so they are
  human-only for MCP.
- **Placed Elements remember their entry.** `element.place` records the entry in the Element's
  config (`nodecraft.library_entry`). `element.update` uses it, so an Element placed as `Hero-2`,
  or renamed on the canvas afterwards, updates from `Hero`. If that entry is gone, update says
  so rather than taking another entry that happens to share the Element's name. Elements placed
  before this change, with nothing recorded, still go by their name.
- **A damaged entry is set aside, not fatal.** An entry the build can't use (an unknown field or
  category from a newer build, a bad name, or a second entry with the same name) is kept as it was
  and reported by `library.elements` under `damaged`, with its index, name, reason and the library
  file. The rest of the library works. Writing the library back keeps the damaged entries
  unchanged, and media cleanup is skipped while any exist, since they may show images. A file that
  can't be read at all is still refused, and the error now names the file.
- **The panel does the rest.** The Elements panel renames inline (Enter renames; Escape or
  clicking away keeps the name), toggles Shared, exports through the save dialog, imports through
  the file picker, and asks before Remove.
