## 2026-10-22 · A Layer Editor card keeps its own fonts (#548)

- **Fonts belong to the card.** An imported font lives in the Layer Editor node's config as a list
  of `[name, asset]` pairs, which the planner copies into the job. It is not a project-wide setting.
  As a result, a copied card, a published technique and an undo all carry the fonts the card draws
  with, and the run needs nothing from outside its job. The cost: a font used on two cards is added
  to each, although the bytes are stored only once.
- **A font is named by what it declares.** The name is the typographic family (else the family),
  plus the style when that isn't Regular ("DejaVu Serif Bold Italic"). A text layer names a font by
  that name, ignoring case and surrounding space. A font of the same name replaces the one already
  held, so a newer file is one add. The built-in Lato can't be shadowed by a font of the same name.
- **Missing means Lato, never an error.** A layer naming a font the card doesn't hold, or holds
  but can't read, draws with Lato in the editor and in a run alike, and the editor says so. A
  missing font never blocks a run that may already have cost money upstream.
- **Only single TrueType or OpenType fonts.** `font_info` checks the magic bytes and requires
  outlines, a character map and a name. Collections (`.ttc`), bitmap-only fonts and anything else
  are refused before the file is stored. Each card holds at most 16 fonts of at most 16 MiB each.
- **Adding is one command.** `layers.font_add` takes an imported asset. The desktop editor reads
  the chosen file off the frame, imports it and dispatches the command
  (`layer_cmds::fonts::import`). Setting the new font on the layer it was picked for is a second
  undo step. The browser build has no font picker yet; agents and the CLI use the command.
