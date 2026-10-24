## 2026-10-20 · The technique library is one file of published versions every project shares (#96)

- **One file in the settings folder.** The library is `library/techniques.json` beside
  `plugins/` in the app's settings folder: a versioned JSON file listing the published technique
  versions kept, each a whole technique canvas. It is written atomically, under a `.lock` file so
  two windows never both write it; a window that finds it locked says the library is busy. The
  browser and headless builds keep it in memory for the session. A library that is damaged, too
  large (64 MiB, 1024 versions, 1 MiB a version) or from a newer format is reported and left
  untouched, never rewritten.
- **A version number names one content.** A technique id and version number appear at most once
  in the library, as in a project. Adding a version the library already has keeps the one there
  (`added: false`); placing a version a project already has uses the project's own copy, and a
  version the project removed can't be placed again until that is undone.
- **The library keeps the workflow, not its results.** `library.add` stores the version without
  the canvas it was published from or its nodes' results, except each input's example (without
  the settings it was made with), which a placed node uses while nothing feeds that input. A
  version that holds media can't be added yet, since the library has no asset store of its own;
  nor can one that runs other techniques, whose versions would have to come along. Reading the
  file applies the same rules, so a hand-edited file can't bring in what adding refuses.
- **Placing copies the version into the project.** `library.place` adds a copy of the version as
  a new technique canvas, its nodes and wires under ids derived from the copy's id, and one node
  running it, as one undo step. The project never refers back to the library, so removing a
  version from the library leaves every project that placed it working.
- **The host does the I/O.** Listing, adding and removing go through the `library.store` host
  service, which re-reads the file before each change; placing reads the library as the service
  last read it, so a caller lists before placing. Agents may list and place; only a person adds
  or removes, since the library is shared and outside undo. The CLI's MCP server reads the
  app's library and never creates it.
- **The Techniques panel** lists the project's published versions (Place, Add to library) and the
  library's (Place, Remove); a node is placed in the middle of the view and selected.

Not yet: asset and history browsers across projects, dragging from the panel onto a canvas, and
starter templates shipped as techniques.
