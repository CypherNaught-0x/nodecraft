## 2026-10-23 · Copying a result to the clipboard goes through a typed clipboard host (#831)

- **The engine reads, the host only owns the clipboard.** `history.copy {canvas, id, generation,
  port?, item?}` picks a text or image result like `history.export` does. Its effect
  (`Effect::Copy`) reads the image from the project, checks it against the stored hash, decodes it
  upright and hands straight RGBA to the host registered with `Session::register_clipboard_host`.
  Decoding in the engine keeps the caps in one place and lets any frontend copy pixels without
  its own decoder.
- **Caps:** at most 64 MiB encoded, 8192 pixels a side (256 MiB of RGBA) and 512 MiB while
  decoding, so a browser tab never runs out of memory; anything larger is refused and pointed at Export. Video, audio, 3D and documents are refused the same way; there is
  no portable clipboard format for them.
- **No clipboard, no command.** A session without a clipboard host (the CLI, MCP, headless runs)
  reports `history.copy` as unsupported, so it is disabled rather than pretending to copy.
- **The app hands the clip to egui at the end of the frame** (`ctx.copy_text` / `ctx.copy_image`);
  eframe writes it with arboard on the desktop and through the browser's clipboard on the web.
  The pixels go to egui byte for byte (`from_rgba_premultiplied`), because both backends pass
  `Color32`'s bytes on as straight RGBA. eframe only logs a clipboard that refuses (a browser
  outside a secure context), so the "Copied" notice says what was sent, not what arrived.
- **Anyone who can see a result can copy it**, view-only participants of a shared project
  included, as with Export: it reads the result and changes nothing.
- **Human-only over MCP:** what lands on the clipboard is what the person pastes next.
