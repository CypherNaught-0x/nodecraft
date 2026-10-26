## 2026-10-24 · A folder becomes one Folder card that loops like a batch

- **One card, files imported.** A folder dropped on the canvas, or picked with **Import
  folder…** (the Project files window), becomes one `source.folder` card. Its files are recognised
  from their bytes, as dropped files are, and its media go into the project's asset store, so a
  project keeps working when the folder moves or changes and sharing a project never shares a
  path. The card keeps no path on disk; **Import again…** on the card, or dropping a folder on it,
  replaces its files (`folder.fill`) and keeps its settings and wires. One undo step either way.
- **One kind of file per card.** As a batch carries one modality, a folder holds the kind most of
  its files are (`config["modality"]`, image when absent); files of other kinds are left out with
  a reason. A wired card keeps its kind when filled again, since another kind changes its ports.
- **Files live in config, settings in parameters.** `config["names"]` (paths inside the folder,
  `/`-separated, unique) and `config["files"]` (their values) are parallel lists in import order,
  at most 1,000 files (a batch's limit). Filters are ordinary parameters, so they are undoable,
  editable in bulk and part of the staleness hash: names to keep and to skip (`*`/`?` wildcards,
  `;`-separated, case ignored; a pattern with `/` matches the path, others the file name), an order
  (natural, alphabetical, as imported, or a seeded shuffle that doesn't depend on import order),
  reverse, skip the first N, take every Nth, at most N, and what names give (file name, without
  extension, or path). Matching is bounded: each pass lowercases each name once and stops with
  an error past 4 million matching steps, so a hostile project's filters can't stall the canvas.
- **Four outputs.** `file` and `name` loop: the planner reads them as a batch's items, so every
  node with a single input fed by them runs once per file, and a list input collects them. Both
  share the files' item ids, so a node fed by a file and its name runs once per file, not N × N.
  Their `pairing` setting pairs them with batches or other folders as a batch's does. `files` and
  `names` give every file or name at once as one list. A batch collects a folder's lists, not its
  looping outputs; mentions don't read a folder (it gives several values). A card whose filters
  keep no file blocks what it feeds, with that reason, rather than giving an empty list.
- **Reading is bounded and off the canvas frame.** A folder is read on its own thread: hidden
  entries and system clutter skipped, no link followed (one to a folder could loop, one to a file
  could bring in something from outside the folder, such as a key file), subfolders 8 levels
  deep, at most 20,000 entries looked at and the first 1,000 files (natural order) read, each at
  most 16 MiB through `File::take` after a regular-file check, at most 256 MiB in all. It can be
  cancelled; nothing is added until it is read.
- **The import is synchronous and bounded.** Recognising, probing, hashing and storing the read
  files then runs once on the UI thread, as a drop's does, but for up to 1,000 files and 256 MiB
  rather than 32 files and 48 MiB, so a large folder can pause the window for a few seconds.
  Moving that work off the UI thread is left for later. The card is added in one command, whose
  text is bounded, so file names, text and each entry's framing share a 960 KiB budget; files past
  it are left out with a reason instead of failing the import. One folder can use up to 1,000 of
  a project's 1,024 asset slots.
- **Not yet.** Browser drops of folders (the web build reads drops asynchronously and has no
  folder entries yet), refreshing from the same path without picking it again, and a CLI or MCP
  way to read a folder from disk (`folder.fill` takes media already in the project).
