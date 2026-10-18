## 2026-10-18 · Files dropped from the OS (#197)

- **A drop becomes source nodes in one undo step.** `engine::import::import_files` recognises each
  file from its bytes (`media::sniff`), never its name. UTF-8 and UTF-16 text becomes a Text node
  (up to the node's 1 MiB). Images, video and audio go into the project's asset store and become
  Image, Video and Audio nodes that reference it; images record their size. All of them are
  added through one `canvas.paste`.
- **What can't be held is skipped with a reason, not an error:** empty or unrecognised files,
  text over 1 MiB, media over the 16 MiB per-asset cap, images that don't decode, and 3D models
  and documents (no source kinds yet). The rest of the drop still arrives, and one notice names
  the first skipped file and counts the others. A drop adds at most 32 files.
- **Reads are bounded before they happen:** the shell checks a dropped file's size from its
  metadata and refuses anything over 16 MiB without reading it. egui's native read takes the
  whole file, so this check is what keeps a huge drop from filling memory.
- **Placement and selection:** the first card is centred under the pointer and the rest follow
  in a row. The new nodes become the selection. While files hover over the window, it shows
  "Drop to add as nodes".
- **Not yet:** pasting image data from the OS clipboard. egui 0.36 delivers clipboard pastes as
  text only, so this waits for an integration that exposes image data. Browser drops need an
  asynchronous read, so in the web build a drop shows a notice for now.
