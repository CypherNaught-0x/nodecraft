## 2026-10-27 · Preview cache admission and failure retention (#396)

Refines the bounded cache of [image previews on cards](2026-10-18-image-previews-on-cards.md),
which allocated a texture first and evicted afterwards, never evicting one used this frame, so
97 same-frame 512 px previews held 97 MiB, and kept decode failures forever.

- **Admit before reading.** A decode at edge `e` reserves `e × e × 4` bytes (thumbnails and PDF
  pages fit within `e`). Textures plus the reservations of decodes in flight never exceed
  96 MiB. Room is made only by dropping least-recently-used textures drawn neither this frame
  nor the last: a card drawn early in a frame must not push out one still on screen that is
  drawn later (which would then push out another, a cascade of flicker and re-decodes). If that
  can't make room, nothing is read or decoded and the card says "Preview cache full", or shows a
  smaller ready size of the same image, which is pinned before the sharper size is admitted.
- **A refusal asks for one more frame.** Cards that left the view were still drawn last frame, so
  they become evictable in the next one; the refusal requests it. A frame that was itself that
  retry and had no input asks for no more, so a visible set larger than the budget stays partly
  uncached without repainting forever. (A view that changes without input in exactly that retry
  frame waits for the next frame.)
- **Reservations belong to a request ticket.** Each worker job carries a ticket; a completion
  releases a reservation only when it matches the pending entry. A worker that disconnects
  releases every reservation and its images are asked for again.
- **Bounded metadata.** At most 1024 entries in all, and at most 256 remembered failures
  (decodes in flight hold a failure slot, so their completions always fit). Past either cap,
  entries drawn neither this frame nor the last go first; when none is left, new requests get
  "Preview cache full" without reading. Because every admission needs a failure slot, about 250
  or more broken images on screen at once block all new previews, good ones included, until
  they leave the view; that many visible broken images is accepted as the price of never
  decoding them repeatedly.
- **Failures expire when idle.** A failure is kept while it is still asked for, so a visible
  broken image is decoded once. One that wasn't asked for in the last frame is dropped after one
  second, so it is tried again if it returns. The cache schedules the repaints this needs, without
  input: while failures are on screen, one check is pending at a time, a second after the frame
  that armed it, and the check frame itself arms none; plus one wake at each idle failure's
  expiry. A stable view therefore stops asking for frames.
- **What the bound covers.** Texture bytes and reservations owned by the cache, and its entry
  counts. It is not a process memory limit: the decoder's working memory (bounded by its own
  limits), egui's upload copy, GPU driver allocations and the encoded bytes being read are not
  counted.
