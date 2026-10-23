## 2026-10-23 · Custom shaders are saved to their own versioned library (#90)

- **A library of its own, not techniques.** A shader card's input is an image, which a technique
  would have to keep as media, and the technique library refuses media. Publishing and keeping
  are also two commands there, and a technique would freeze the card. So shaders have their own
  library, `shaders.json` in the app's data folder (`shaders/`, beside `library/` and `elements/`,
  with a lock of its own), kept in memory in the browser. It holds only the shader: the WGSL and
  its declared parameters. No media, keys or project data.
- **Names with numbered versions.** A shader is a name (1 to 40 characters on one line, unique
  ignoring case) with up to 64 versions. `shader.save {canvas, id, name?}` keeps the card's shader
  as the name's next version, unless the latest is already that shader. Numbers only go up:
  removing a version never frees its number, so a version number always means one shader. Removing a
  whole shader remembers its next number (up to 4096 removed names), so saving the name again carries
  on from there. The file is checked when read (names, order, sizes), and a damaged file is
  reported where the library is shown, never overwritten.
- **A card records where its shader came from.** `shader.place {canvas, name, version?, id, pos}`
  adds a card with that version, titled with its name. `shader.update {canvas, id, name?, version?}`
  gives a card a version (by default the latest of the one it came from) as one undo step. Both
  record `{name, version}` in the card's config (`nodecraft.shader.from`). A hand edit keeps that
  record, and the editor shows it and offers "Update to vN" when the library has a newer version.
  The record names a source, not a recipe, so it is left out of the staleness hash. A new
  version's code is in the hash, so its results go stale.
- **Saving and removing are a person's.** The library is shared by every project and outside
  undo, so `shader.save` and `library.remove_shader` are human-only on MCP and the control
  channel, as for techniques and Elements. Agents may list (`library.shaders`), place and update.
  The CLI reads the app's library.
- **UI.** The shader editor has "Save as" with "Save to library" (enabled for an applied, valid
  shader), and shows the version the card came from. The Techniques panel lists the library's
  shaders with Place (latest version) and Remove.
- **Not here.** Shaders with more than one image input (#162). Custom text transforms are #165's
  bounded expressions. Audio and cross-media transforms aren't shaders: they wait for a bounded
  host operation or an isolated runtime (#154), never generated scripts.
