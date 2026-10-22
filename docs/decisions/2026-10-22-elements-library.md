## 2026-10-22 · The Elements library is one file plus its images, and placing copies them into the project (#552)

- **One file and a media folder in the settings folder.** The library is `elements/elements.json`
  (a versioned JSON file, one entry per Element name, matched case-insensitively). Beside it,
  `elements/element-media/<blake3>` holds the images its entries show. Writes are atomic and made
  under `elements.lock`, so two windows never both write; a window that finds the library locked
  says it is busy. A library that is damaged, too large (16 MiB, 1024 entries) or from a newer
  format is reported and left untouched. The browser and headless builds keep the library in
  memory for the session.
- **An entry keeps an Element's settings, not its wiring.** An entry holds the name, category,
  guidelines, the wired images in order, the reference sheet and a `shared` flag. Saving needs
  every wired image to be ready. Saving over an existing name needs `replace`. Media that no entry
  shows any more is deleted after each change; any file in the media folder not named by a hash is
  left alone.
- **Library media is checked like project media.** Every read checks the image against its hash,
  and placing imports it through the same checks as a dropped file. Placing or updating copies
  any missing images into the project's asset store and makes the graph change in one undo step,
  through a `Place` effect. A copy that fails changes nothing. Projects never refer back to the
  library, so removing an entry leaves every project that placed it working.
- **Placing makes image sources.** `element.place` adds the Element and one `source.image` node
  for each image, plus one for the sheet wired to its sheet port. If the Element's name is taken
  on the canvas, the call is refused; the caller passes `as` with a free name (the panel picks
  `name-2`, `name-3`, and so on). `element.update` swaps in the entry's images, category and
  guidelines. It removes image sources that fed only this Element and unwires the others.
- **Reads are as last listed.** `library.elements` reads the file again. Placing and updating
  use the library as this window last read it, as the technique library does, so a caller lists
  before placing.
- **Agents list, place and update; only a person saves, removes or renames.** The library is
  shared and outside undo.
- **Update goes by name.** `element.update` takes the library entry with the Element's own name;
  nothing on the node records where it came from. So an Element placed under another name
  (`Hero-2`) is updated only after it is renamed back.
- **The `shared` flag is recorded but not acted on yet.** Syncing shared Elements to a team and
  exporting them are follow-ups. The Elements panel has no rename yet; `library.rename_element`
  is a command.
