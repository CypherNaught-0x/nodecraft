## 2026-10-18 · Files dropped from the OS (#197)

- **A drop becomes source nodes in one undo step.** `engine::import::import_files` recognises each
  file from its bytes (`media::sniff`), never its name. UTF-8 and UTF-16 text becomes a Text node
  (up to the node's 1 MiB). Images, video and audio go into the project's asset store and become
  Image, Video and Audio nodes that reference it; images record their size. All of them are
  added through one `canvas.paste`.
- **What can't be held is skipped with a reason, not an error.** The rest of the drop still
  arrives, and one notice names the first skipped file and counts the others. Skipped files are:
  - empty or unrecognised files, and text with binary data anywhere in it;
  - text past the drop's 960 KiB text budget (shared by all its files, since a paste is one
    command and a command's strings share 1 MiB);
  - media over the 16 MiB per-asset cap, or past the room left in the project's 48 MiB store;
  - images that don't decode (HEIC, HEIF and AVIF are kept without a size until they can be);
  - 3D models and documents, which have no source kinds yet.

  A drop adds at most 32 files.
- **Nothing is stored for a file that won't be added.** Every check runs before any media goes
  into the asset store. If adding the nodes still fails, the drop's assets are pruned
  (`Session::prune_assets`).
- **Reads are bounded however a file behaves.** Only regular files are read, after a size check
  from their metadata, and through `File::take`, so a pipe, a device or a growing file can't
  make the read unbounded. egui's own `bytes()` reads the whole file. One drop reads at most
  48 MiB in total.
- **Placement and selection:** the first card is centred under the pointer and the rest follow
  in a row. The new nodes become the selection. While files hover over the window, it shows
  "Drop to add as nodes".
- **Not yet:** pasting image data from the OS clipboard. egui 0.36 delivers clipboard pastes as
  text only, so this waits for an integration that exposes image data. Browser drops need an
  asynchronous read, so in the web build a drop shows a notice for now.
